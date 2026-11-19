//! User controls and pending human input. Authorization remains in core.
use super::*;
use crate::{
    i18n::{common, controller as text, view as ui_text},
    view::{Details, MarkdownDisplay},
};
use soyo_app::interaction::Request;
use soyo_core::{config::ThinkingEffort, contract::PermissionMode, execution::BackendKind};
use std::path::Path;

fn mode(value: &str) -> Option<WorkMode> {
    match value.to_ascii_lowercase().as_str() {
        "plan" => Some(WorkMode::Plan),
        "build" => Some(WorkMode::Build),
        "code" => Some(WorkMode::Code),
        "work" => Some(WorkMode::Work),
        _ => None,
    }
}
fn permission(value: &str) -> Option<PermissionMode> {
    match value.to_ascii_lowercase().as_str() {
        "auto" => Some(PermissionMode::Auto),
        "manual" => Some(PermissionMode::Manual),
        "bypass" => Some(PermissionMode::Bypass),
        _ => None,
    }
}
fn effort(value: &str) -> Option<Option<ThinkingEffort>> {
    match value.to_ascii_lowercase().as_str() {
        "low" => Some(Some(ThinkingEffort::Low)),
        "medium" => Some(Some(ThinkingEffort::Medium)),
        "high" => Some(Some(ThinkingEffort::High)),
        "xhigh" => Some(Some(ThinkingEffort::XHigh)),
        "max" => Some(Some(ThinkingEffort::Max)),
        "default" => Some(None),
        _ => None,
    }
}
fn token_count(value: &str) -> Option<u64> {
    let value = value.trim().to_ascii_lowercase();
    let (number, multiplier) = match value.as_bytes().last().copied() {
        Some(b'k') => (&value[..value.len() - 1], 1_000u64),
        Some(b'm') => (&value[..value.len() - 1], 1_000_000u64),
        _ => (value.as_str(), 1u64),
    };
    number
        .parse::<u64>()
        .ok()
        .filter(|tokens| *tokens > 0)
        .and_then(|tokens| tokens.checked_mul(multiplier))
}
pub(super) fn closed(request: &Request) -> bool {
    match request {
        Request::Approval(_, reply) => reply.is_closed(),
        Request::Input(_, reply) => reply.is_closed(),
    }
}
impl App {
    pub(super) fn require_idle_configuration(&self) -> Result<()> {
        if self.active.is_some() || self.ui_job.is_some() {
            return Err(text::IDLE_ONLY.into());
        }
        if self.pending.is_some() {
            return Err(text::PENDING_REQUEST.into());
        }
        Ok(())
    }
    async fn change_workspace(&mut self, argument: &str) -> Result<()> {
        self.require_idle_configuration()?;
        if argument.is_empty() {
            self.notice(&text::workspace(&self.config.cwd.to_string_lossy()));
            return Ok(());
        }
        let requested = Path::new(argument);
        let path = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            self.config.cwd.join(requested)
        };
        let workspace = std::fs::canonicalize(path).map_err(|_| text::WORKSPACE_MISSING)?;
        if !workspace.is_dir() {
            return Err(text::WORKSPACE_NOT_DIRECTORY.into());
        }
        session::check_roots(&self.config, &workspace, self.current_mode()?)?;
        let previous = self.config.cwd.clone();
        self.config.cwd = workspace.clone();
        if let Some(prepared) = &mut self.prepared {
            let fingerprint = self
                .config
                .profiles
                .get(&prepared.session().profile)
                .ok_or(text::SESSION_PROFILE_NOT_FOUND)?
                .fingerprint()?;
            if let Err(error) = prepared.set_workspace(&workspace, &fingerprint).await {
                self.config.cwd = previous;
                return Err(error);
            }
        }
        self.state.details = None;
        self.state.details_scroll = 0;
        self.state.skills_count = None;
        self.refresh_header();
        self.notice(text::WORKSPACE_CHANGED);
        Ok(())
    }
    async fn select_effort(&mut self, argument: &str) -> Result<()> {
        self.require_idle_configuration()?;
        let name = self.config.profile.clone().ok_or(text::PROFILE_REQUIRED)?;
        let current = self
            .config
            .profiles
            .get(&name)
            .and_then(|profile| profile.thinking_effort)
            .map_or("default", ThinkingEffort::as_str);
        if argument.is_empty() {
            self.notice(&text::thinking_effort(current));
            return Ok(());
        }
        let selected = effort(argument).ok_or(text::EFFORT_USAGE)?;
        let previous = self
            .config
            .profiles
            .get(&name)
            .ok_or(text::PROFILE_UNAVAILABLE)?
            .thinking_effort;
        let fingerprint = {
            let profile = self
                .config
                .profiles
                .get_mut(&name)
                .ok_or(text::PROFILE_UNAVAILABLE)?;
            profile.thinking_effort = selected;
            profile.fingerprint()?
        };
        if let Some(prepared) = &self.prepared
            && let Err(error) = prepared.update_config_fingerprint(&fingerprint).await
        {
            if let Some(profile) = self.config.profiles.get_mut(&name) {
                profile.thinking_effort = previous;
            }
            return Err(error);
        }
        if let Some(client) = &self.helper
            && let Err(error) = client
                .call(
                    "set_thinking_effort",
                    serde_json::json!({"effort": selected.map(ThinkingEffort::as_str)}),
                )
                .await
        {
            if let Some(profile) = self.config.profiles.get_mut(&name) {
                profile.thinking_effort = previous;
            }
            return Err(Box::new(BoundaryError(
                soyo_core::contract::ErrorInfo {
                    code: error.code.clone(),
                    message: error.message.clone(),
                },
                None,
                None,
            )));
        }
        self.refresh_header();
        let value = selected.map_or("default", ThinkingEffort::as_str);
        self.notice(&text::thinking_effort_set(value));
        Ok(())
    }
    /// Mirror a local-only `Config` change onto the helper's configuration.
    /// No-op on the in-process backend.
    async fn sync_remote_config(&self, method: &str, params: serde_json::Value) -> Result<()> {
        let Some(client) = &self.helper else {
            return Ok(());
        };
        client.call(method, params).await.map_err(|error| {
            Box::new(BoundaryError(
                soyo_core::contract::ErrorInfo {
                    code: error.code.clone(),
                    message: error.message.clone(),
                },
                None,
                None,
            )) as Box<dyn std::error::Error>
        })?;
        Ok(())
    }

    async fn set_sandbox(&mut self, argument: &str) -> Result<()> {
        match argument.to_ascii_lowercase().as_str() {
            "" | "status" => {
                self.notice(&text::sandbox(sandbox_label(self.config.sandbox.backend)));
            }
            "on" => {
                self.require_idle_configuration()?;
                self.sync_remote_config(
                    "set_sandbox_backend",
                    serde_json::json!({"backend": "auto"}),
                )
                .await?;
                self.config.sandbox.backend = BackendKind::Auto;
                self.notice(text::SANDBOX_ON);
            }
            "off" => {
                self.require_idle_configuration()?;
                self.sync_remote_config(
                    "set_sandbox_backend",
                    serde_json::json!({"backend": "host"}),
                )
                .await?;
                self.config.sandbox.backend = BackendKind::Host;
                self.notice(text::SANDBOX_OFF);
            }
            _ => return Err(text::SANDBOX_USAGE.into()),
        }
        Ok(())
    }
    async fn set_autocompact(&mut self, argument: &str) -> Result<()> {
        self.require_idle_configuration()?;
        match argument.to_ascii_lowercase().as_str() {
            "" => {
                let value = if !self.config.context.auto_compact {
                    text::AUTOCOMPACT_STATUS_OFF.into()
                } else if let Some(tokens) = self.config.context.trigger_tokens {
                    text::autocompact_on_tokens(tokens)
                } else {
                    text::autocompact_on_percent(self.config.context.trigger_percent)
                };
                self.notice(&text::autocompact(&value));
            }
            "auto" => {
                self.sync_remote_config(
                    "set_autocompact",
                    serde_json::json!({"enabled": true, "trigger_tokens": null}),
                )
                .await?;
                self.config.context.auto_compact = true;
                self.config.context.trigger_tokens = None;
                self.notice(&text::autocompact_auto(self.config.context.trigger_percent));
            }
            "off" => {
                self.sync_remote_config("set_autocompact", serde_json::json!({"enabled": false}))
                    .await?;
                self.config.context.auto_compact = false;
                self.notice(text::AUTOCOMPACT_OFF);
            }
            value => {
                let tokens = token_count(value).ok_or(text::AUTOCOMPACT_USAGE)?;
                let profile = self
                    .config
                    .profile
                    .as_ref()
                    .and_then(|name| self.config.profiles.get(name))
                    .ok_or(text::PROFILE_REQUIRED)?;
                let budget = self.config.context.budget(profile);
                let capacity = budget.input_capacity()?;
                let target = ((u128::from(capacity)
                    * u128::from(self.config.context.target_percent))
                    / 100) as u64;
                let effective = tokens.min(capacity);
                if effective <= target {
                    return Err(text::AUTOCOMPACT_TARGET_THRESHOLD.into());
                }
                self.sync_remote_config(
                    "set_autocompact",
                    serde_json::json!({"enabled": true, "trigger_tokens": effective}),
                )
                .await?;
                self.config.context.auto_compact = true;
                self.config.context.trigger_tokens = Some(effective);
                self.notice(&text::autocompact_set(effective));
            }
        }
        Ok(())
    }
    pub(super) async fn cycle_mode(&mut self) -> Result<()> {
        let selected = if let Some(mode) = self.deferred_mode {
            mode
        } else if let Some(active) = &self.active {
            let snapshot = active.handle.mode()?;
            snapshot.pending.unwrap_or(snapshot.current)
        } else {
            self.config.mode
        };
        let target = match selected {
            WorkMode::Work => WorkMode::Code,
            WorkMode::Code => WorkMode::Plan,
            WorkMode::Plan => WorkMode::Build,
            WorkMode::Build => WorkMode::Work,
        };
        self.request_mode(target, false).await?;
        Ok(())
    }
    async fn request_mode(&mut self, target: WorkMode, announce: bool) -> Result<()> {
        let workspace = self
            .prepared
            .as_ref()
            .map(|prepared| prepared.session().workspace.as_path())
            .unwrap_or(&self.config.cwd);
        session::check_roots(&self.config, workspace, target)?;
        if let Some(job) = &self.ui_job {
            job.cancel.cancel();
            self.deferred_mode = Some(target);
            self.state.pending_mode = Some(format!("{target:?}").to_uppercase());
            self.state.details = None;
            self.notice(text::MODE_CHANGE_UI);
            return Ok(());
        }
        if let Some(active) = &self.active {
            active.handle.request_mode(target).await?;
            self.invalidate_mode_approval();
            self.refresh_mode()?;
        } else {
            if let Some(client) = &self.helper
                && self.prepared.is_some()
            {
                client
                    .call(
                        "set_work_mode",
                        serde_json::json!({"mode": soyo_ipc::types::WorkModeDto::from(target)}),
                    )
                    .await
                    .map_err(|error| {
                        Box::new(BoundaryError(
                            soyo_core::contract::ErrorInfo {
                                code: error.code.clone(),
                                message: error.message.clone(),
                            },
                            None,
                            None,
                        )) as Box<dyn std::error::Error>
                    })?;
            }
            if let Some(prepared) = &mut self.prepared {
                let mut updated = prepared.session().clone();
                updated.work_mode = target;
                prepared.commit_session(updated).await?;
            }
            self.config.mode = target;
            self.state.pending_mode = None;
            self.refresh_header();
        }
        if announce {
            self.notice(&text::mode_requested(&format!("{target:?}")));
        }
        Ok(())
    }
    pub(super) fn show_pending(&mut self) {
        let Some(request) = &self.pending else { return };
        let (title, lines) = match request {
            Request::Approval(request, _) => {
                let mut lines = vec![
                    text::approval_field(text::TOOL_LABEL, &request.tool),
                    text::approval_field(text::CALL_LABEL, &request.call_id),
                    text::approval_field(text::APPROVAL_LABEL, &request.approval_id),
                    text::approval_field(text::MODE_LABEL, format_args!("{:?}", request.work_mode)),
                    text::approval_field(text::WORKSPACE_LABEL, request.cwd.display()),
                ];
                if let Some(action) =
                    crate::tool_render::approval_action(&request.tool, &request.arguments)
                {
                    lines.push(text::approval_field(crate::i18n::browser::ACTION, action));
                }
                lines.push(String::new());
                lines.push(request.operation_summary.clone());
                lines.push(String::new());
                lines.extend(
                    crate::tool_render::call(&request.tool, &request.arguments, None)
                        .lines()
                        .map(str::to_owned),
                );
                lines.push(String::new());
                lines.push(text::APPROVAL_HELP.into());
                (crate::i18n::view::APPROVAL_REQUIRED, lines)
            }
            Request::Input(request, _) => (
                crate::i18n::view::QUESTION,
                std::iter::once(request.question.clone())
                    .chain(
                        request
                            .options
                            .iter()
                            .enumerate()
                            .map(|(i, text)| format!("{}. {text}", i + 1)),
                    )
                    .chain(std::iter::once(text::INPUT_HELP.into()))
                    .collect(),
            ),
        };
        self.state.set_details(Details {
            title: title.into(),
            lines: lines.iter().map(|text| self.safe(text)).collect(),
        });
        self.state.details_scroll = 0;
        self.state.status = if matches!(request, Request::Approval(..)) {
            common::AWAITING_APPROVAL
        } else {
            common::AWAITING_INPUT
        }
        .into();
    }
    pub(super) fn request(&mut self, request: Request) {
        if closed(&request) {
            return;
        }
        self.pending = Some(request);
        self.show_pending();
    }
    pub(super) fn invalidate_pending(&mut self) {
        self.pending = None;
        self.state.details = None;
        self.state.details_scroll = 0;
    }
    fn invalidate_mode_approval(&mut self) {
        if matches!(self.pending, Some(Request::Approval(..))) {
            if let Some(Request::Approval(_, reply)) = self.pending.take() {
                let _ = reply.send(Err(BoundaryError(
                    soyo_core::contract::ErrorInfo {
                        code: "stale_approval".into(),
                        message: text::WORK_MODE_CHANGED.into(),
                    },
                    None,
                    None,
                )));
            }
            self.invalidate_pending();
        } else {
            self.show_pending();
        }
    }
    pub(super) fn reply_approval(&mut self, approved: bool) {
        if let Some(Request::Approval(_, reply)) = self.pending.take() {
            let _ = reply.send(Ok(Some(approved)));
            self.state.details = None;
            self.state.status = common::RUNNING.into();
        }
    }
    pub(super) fn refresh_mode(&mut self) -> Result<bool> {
        let mut changed = false;
        if let Some(active) = &self.active {
            let snapshot = active.handle.mode()?;
            let current = format!("{:?}", snapshot.current).to_uppercase();
            let pending = snapshot
                .pending
                .map(|value| format!("{value:?}").to_uppercase());
            let pending = pending.or_else(|| {
                self.deferred_mode
                    .map(|mode| format!("{mode:?}").to_uppercase())
            });
            changed = self.state.mode != current || self.state.pending_mode != pending;
            self.state.mode = current;
            self.state.pending_mode = pending;
        }
        if self.active.is_none() {
            let pending = self
                .deferred_mode
                .map(|mode| format!("{mode:?}").to_uppercase());
            changed |= self.state.pending_mode != pending;
            self.state.pending_mode = pending;
        }
        if self.pending.as_ref().is_some_and(closed) {
            self.invalidate_pending();
            self.notice(text::PENDING_EXPIRED);
            changed = true;
        }
        Ok(changed)
    }
    pub(super) async fn submit(&mut self) -> Result<bool> {
        let text = self.state.editor.text().to_owned();
        if text.starts_with('/') && !text.starts_with("//") {
            let exit = self.command(&text).await?;
            self.clear_editor();
            return Ok(exit);
        }
        if self.pending.is_some() && self.deferred_mode.is_some() {
            self.notice(text::PENDING_MODE);
            return Ok(false);
        }
        let prompt_title = match &self.pending {
            Some(Request::Approval(..)) => Some(crate::i18n::view::APPROVAL_REQUIRED),
            Some(Request::Input(..)) => Some(crate::i18n::view::QUESTION),
            None => None,
        };
        if let Some(title) = prompt_title
            && !self
                .state
                .details
                .as_ref()
                .is_some_and(|details| details.title == title)
        {
            self.show_pending();
            self.notice(text::REVIEW_REQUEST);
            return Ok(false);
        }
        if let Some(request) = self.pending.take() {
            match request {
                Request::Input(_, reply) if !text.trim().is_empty() => {
                    let answer = if text.starts_with("//") {
                        text[1..].to_owned()
                    } else {
                        text
                    };
                    let expanded = self.expand_attachments(&answer);
                    let _ = reply.send(Ok(Some(expanded)));
                    self.clear_editor();
                    self.state.details = None;
                    self.state.status = common::RUNNING.into();
                }
                Request::Approval(request, reply) => {
                    match text.trim().to_ascii_lowercase().as_str() {
                        "y" | "yes" => {
                            let _ = reply.send(Ok(Some(true)));
                            self.clear_editor();
                            self.state.details = None;
                            self.state.status = common::RUNNING.into();
                        }
                        "n" | "no" => {
                            let _ = reply.send(Ok(Some(false)));
                            self.clear_editor();
                            self.state.details = None;
                            self.state.status = common::RUNNING.into();
                        }
                        _ => {
                            if self
                                .active
                                .as_ref()
                                .is_some_and(|active| active.kind == RunKind::Execution)
                                && !text.trim().is_empty()
                            {
                                let guidance = if text.starts_with("//") {
                                    text[1..].to_owned()
                                } else {
                                    text
                                };
                                let display = self.safe(&guidance);
                                let expanded = self.expand_attachments(&guidance);
                                self.active
                                    .as_ref()
                                    .ok_or(text::ACTIVE_RUN_REQUIRED)?
                                    .handle
                                    .steer(expanded)
                                    .await?;
                                self.pending = Some(Request::Approval(request, reply));
                                self.state.push_message(Role::User, ui_text::YOU, display);
                                self.notice(text::GUIDANCE_APPROVAL_PENDING);
                                self.clear_editor();
                            } else {
                                self.pending = Some(Request::Approval(request, reply));
                                self.notice(text::APPROVE_OR_CONTROL);
                            }
                        }
                    }
                }
                request => self.pending = Some(request),
            }
            return Ok(false);
        }
        if self.active.is_some() {
            let guidance = if text.starts_with("//") {
                text[1..].to_owned()
            } else {
                text
            };
            if guidance.trim().is_empty() {
                return Ok(false);
            }
            let display = self.safe(&guidance);
            let expanded = self.expand_attachments(&guidance);
            let active = self.active.as_ref().ok_or(text::ACTIVE_RUN_REQUIRED)?;
            if active.kind != RunKind::Execution {
                return Err(text::GUIDANCE_ACTIVE_ONLY.into());
            }
            active.handle.steer(expanded).await?;
            self.state.push_message(Role::User, ui_text::YOU, display);
            self.notice(text::GUIDANCE_QUEUED);
            self.clear_editor();
            return Ok(false);
        }
        if !text.trim().is_empty() {
            let prompt = if text.starts_with("//") {
                text[1..].to_owned()
            } else {
                text
            };
            self.start(prompt).await?;
            self.clear_editor();
        }
        Ok(false)
    }
    pub(super) async fn command(&mut self, command: &str) -> Result<bool> {
        let (name, argument) = command
            .trim()
            .split_once(' ')
            .unwrap_or((command.trim(), ""));
        let argument = argument.trim();
        match name {
            "/exit" => {
                if self.active.is_some() || self.ui_job.is_some() {
                    self.exit_after_run = Some(0);
                    self.cancel();
                } else {
                    return Ok(true);
                }
            }
            "/cancel" => self.cancel(),
            "/compact" => {
                if !argument.is_empty() {
                    return Err(text::COMPACT_NO_ARGS.into());
                }
                self.compact().await?;
            }
            "/memory" => self.open_memory(argument).await?,
            "/mode" => {
                let target = mode(argument).ok_or(text::MODE_USAGE)?;
                self.request_mode(target, true).await?;
            }
            "/permissions" => {
                if self.active.is_some() || self.ui_job.is_some() {
                    return Err(text::PERMISSIONS_IDLE.into());
                }
                let target = permission(argument).ok_or(text::PERMISSIONS_USAGE)?;
                self.sync_remote_config(
                    "set_permission_mode",
                    serde_json::json!({"permissions": soyo_ipc::types::PermissionModeDto::from(target)}),
                )
                .await?;
                self.config.permissions_mode = target;
                self.refresh_header();
                self.notice(text::PERMISSIONS_UPDATED);
            }
            "/cd" => self.change_workspace(argument).await?,
            "/sandbox" => self.set_sandbox(argument).await?,
            "/effort" | "/thinking" => self.select_effort(argument).await?,
            "/autocompact" => self.set_autocompact(argument).await?,
            "/skills" | "/mcps" | "/plugins" => {
                if !argument.is_empty() {
                    return Err(text::command_without_arguments(name).into());
                }
                self.open_picker(&name[1..]).await?
            }
            "/markdown" => {
                let display = match argument {
                    "source" => MarkdownDisplay::Source,
                    "render" => MarkdownDisplay::Render,
                    _ => return Err(text::MARKDOWN_USAGE.into()),
                };
                self.state.set_markdown_display(display);
                self.notice(&text::markdown_display(display.as_str()));
            }
            "/footnote" => {
                let jumped = if argument == "back" {
                    self.state.footnote_back()
                } else {
                    let number = argument
                        .parse::<usize>()
                        .ok()
                        .filter(|number| *number > 0)
                        .ok_or(text::FOOTNOTE_USAGE)?;
                    self.state.jump_footnote(number)
                };
                if !jumped {
                    return Err(text::FOOTNOTE_UNAVAILABLE.into());
                }
                self.state.details = None;
                self.state.details_scroll = 0;
            }
            "/status" => {
                self.refresh_mode()?;
                let session = self
                    .prepared
                    .as_ref()
                    .map(|prepared| prepared.session().id.0.to_string())
                    .unwrap_or_else(|| text::NO_SESSION.into());
                let pending = format!("{:?}", self.state.pending_mode);
                let autocompact = if !self.config.context.auto_compact {
                    text::AUTOCOMPACT_STATUS_OFF.into()
                } else if let Some(tokens) = self.config.context.trigger_tokens {
                    text::status_autocompact_tokens(tokens)
                } else {
                    format!("{}%", self.config.context.trigger_percent)
                };
                self.notice(&text::status_line(
                    &self.state.status,
                    &[
                        (text::STATUS_SESSION, &session),
                        (text::STATUS_MODE, &self.state.mode),
                        (text::STATUS_PENDING, &pending),
                        (text::STATUS_PERMISSIONS, &self.state.permissions),
                        (
                            text::STATUS_SANDBOX,
                            sandbox_label(self.config.sandbox.backend),
                        ),
                        (text::STATUS_AUTOCOMPACT, &autocompact),
                        (text::STATUS_MARKDOWN, self.state.markdown_display.as_str()),
                    ],
                ));
            }
            "/help" => {
                self.state.set_details(Details {
                    title: text::HELP_TITLE.into(),
                    lines: COMMANDS
                        .iter()
                        .map(|command| format!("{} · {}", command.text, command.description))
                        .chain(text::HELP_LINES.iter().copied().map(str::to_owned))
                        .collect(),
                });
                self.state.details_scroll = 0;
            }
            "/ps" => {
                if !argument.is_empty() {
                    return Err(text::command_without_arguments(name).into());
                }
                self.show_processes().await?;
            }
            "/new" | "/profile" | "/model" | "/profiles" | "/resume" | "/resume-accept"
            | "/sessions" | "/tools" | "/jobs" | "/export" | "/export-full" | "/cancel-job"
            | "/goals" | "/goal" => self.service(name, argument).await?,
            _ => return Err(text::UNKNOWN_COMMAND.into()),
        }
        Ok(false)
    }

    async fn show_processes(&mut self) -> Result<()> {
        let lines = if let Some(active) = &self.active {
            match &active.handle {
                ActiveHandle::Local(handle) => background_lines(&handle.background_events()?),
                ActiveHandle::Remote(handle) => {
                    background_lines_remote(&handle.background_jobs().await?)
                }
            }
        } else {
            vec![text::NO_ACTIVE_RUN.into()]
        };
        self.state.set_details(Details {
            title: text::BACKGROUND_TITLE.into(),
            lines: lines.into_iter().map(|line| self.safe(&line)).collect(),
        });
        self.state.details_scroll = 0;
        Ok(())
    }
}

/// `/ps` rendering for helper-reported background jobs; mirrors
/// `background_lines` output.
pub(super) fn background_lines_remote(jobs: &[soyo_ipc::types::BackgroundJobDto]) -> Vec<String> {
    let mut rows: Vec<(String, String, String, String)> = Vec::new();
    for job in jobs {
        let state = match job.state.as_str() {
            "running" | "accepted" => text::BACKGROUND_RUNNING.into(),
            "cancelling" | "cancel_requested" => text::BACKGROUND_CANCELLING.into(),
            "succeeded" => text::BACKGROUND_COMPLETED.into(),
            "cancelled" => text::BACKGROUND_CANCELLED.into(),
            "failed" => job
                .summary
                .as_ref()
                .map(|code| format!("{} · {code}", text::BACKGROUND_FAILED))
                .unwrap_or_else(|| text::BACKGROUND_FAILED.into()),
            other => other.to_owned(),
        };
        rows.push((
            job.job_id.clone(),
            job.tool.clone().unwrap_or_default(),
            job.parent_call_id.clone(),
            state,
        ));
    }
    if rows.is_empty() {
        return vec![text::NO_BACKGROUND_TASKS.into()];
    }
    let running = rows
        .iter()
        .filter(|(_, _, _, state)| state == text::BACKGROUND_RUNNING)
        .count();
    let mut lines = vec![text::background_summary(running, rows.len())];
    for (id, tool, call_id, state) in rows {
        lines.push(String::new());
        lines.push(id);
        lines.push(format!("  {tool} · {call_id}"));
        lines.push(format!("  {state}"));
    }
    lines
}

pub(super) fn background_lines(events: &[soyo_core::background::BackgroundEvent]) -> Vec<String> {
    let mut jobs: Vec<(String, String, String, String)> = Vec::new();
    for event in events {
        match &event.kind {
            soyo_core::background::BackgroundEventKind::Accepted { call_id, tool } => {
                if !jobs.iter().any(|(id, _, _, _)| id == &event.job_id) {
                    jobs.push((
                        event.job_id.clone(),
                        tool.clone(),
                        call_id.clone(),
                        text::BACKGROUND_RUNNING.into(),
                    ));
                }
            }
            soyo_core::background::BackgroundEventKind::CancellationRequested { .. } => {
                if let Some(job) = jobs.iter_mut().find(|(id, _, _, _)| id == &event.job_id) {
                    job.3 = text::BACKGROUND_CANCELLING.into();
                }
            }
            soyo_core::background::BackgroundEventKind::Finished { outcome } => {
                if let Some(job) = jobs.iter_mut().find(|(id, _, _, _)| id == &event.job_id) {
                    job.3 = match outcome {
                        soyo_core::background::BackgroundOutcome::Succeeded => {
                            text::BACKGROUND_COMPLETED.into()
                        }
                        soyo_core::background::BackgroundOutcome::Failed { code } => {
                            format!("{} · {code}", text::BACKGROUND_FAILED)
                        }
                        soyo_core::background::BackgroundOutcome::Cancelled => {
                            text::BACKGROUND_CANCELLED.into()
                        }
                    };
                }
            }
        }
    }
    if jobs.is_empty() {
        return vec![text::NO_BACKGROUND_TASKS.into()];
    }
    let running = jobs
        .iter()
        .filter(|(_, _, _, state)| state == text::BACKGROUND_RUNNING)
        .count();
    let mut lines = vec![text::background_summary(running, jobs.len())];
    for (id, tool, call_id, state) in jobs {
        lines.push(String::new());
        lines.push(id);
        lines.push(format!("  {tool} · {call_id}"));
        lines.push(format!("  {state}"));
    }
    lines
}

fn sandbox_label(backend: BackendKind) -> &'static str {
    match backend {
        BackendKind::Auto => text::SANDBOX_NATIVE,
        BackendKind::Seatbelt => text::SANDBOX_SEATBELT,
        BackendKind::Bubblewrap => text::SANDBOX_BUBBLEWRAP,
        BackendKind::Host => text::SANDBOX_HOST,
    }
}
