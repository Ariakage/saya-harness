//! User controls and pending human input. Authorization remains in core.
use super::*;
use crate::view::Details;
use saya_app::interaction::Request;
use saya_core::{boundary::StoreChange, contract::PermissionMode};

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
pub(super) fn closed(request: &Request) -> bool {
    match request {
        Request::Approval(_, reply) => reply.is_closed(),
        Request::Input(_, reply) => reply.is_closed(),
    }
}
impl App {
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
            .map(|prepared| prepared.session.workspace.as_path())
            .unwrap_or(&self.config.cwd);
        session::check_roots(&self.config, workspace, target)?;
        if let Some(job) = &self.ui_job {
            job.cancel.cancel();
            self.deferred_mode = Some(target);
            self.state.pending_mode = Some(format!("{target:?}").to_uppercase());
            self.state.details = None;
            self.notice("Mode change waits for the UI operation to stop.");
            return Ok(());
        }
        if let Some(active) = &self.active {
            active.handle.request_mode(target)?;
            self.invalidate_mode_approval();
            self.refresh_mode()?;
        } else {
            if let Some(prepared) = &mut self.prepared {
                let mut updated = prepared.session.clone();
                updated.work_mode = target;
                prepared
                    .store
                    .commit(&[StoreChange::Session(updated.clone())])
                    .await?;
                prepared.session = updated;
            }
            self.config.mode = target;
            self.state.pending_mode = None;
            self.refresh_header();
        }
        if announce {
            self.notice(&format!("Mode requested — {target:?}"));
        }
        Ok(())
    }
    pub(super) fn show_pending(&mut self) {
        if self.palette.is_some() {
            self.show_palette();
            return;
        }
        let Some(request) = &self.pending else { return };
        let (title, lines) = match request {
            Request::Approval(request, _) => (
                "Approval required",
                vec![
                    format!("Tool {}", request.tool),
                    format!("Call {}", request.call_id),
                    format!("Approval {}", request.approval_id),
                    format!("Mode {:?}", request.work_mode),
                    format!("Workspace {}", request.cwd.display()),
                    request.operation_summary.clone(),
                    serde_json::to_string_pretty(&request.arguments)
                        .unwrap_or_else(|_| "Unavailable arguments".into()),
                    "y approve · n / Esc deny · /mode MODE changes mode".into(),
                ],
            ),
            Request::Input(request, _) => (
                "Question",
                std::iter::once(request.question.clone())
                    .chain(
                        request
                            .options
                            .iter()
                            .enumerate()
                            .map(|(i, text)| format!("{}. {text}", i + 1)),
                    )
                    .chain(std::iter::once(
                        "Type a reply and press Enter. Ctrl+C cancels.".into(),
                    ))
                    .collect(),
            ),
        };
        self.state.set_details(Details {
            title: title.into(),
            lines: lines.iter().map(|text| self.safe(text)).collect(),
        });
        self.state.details_scroll = 0;
        self.state.status = if matches!(request, Request::Approval(..)) {
            "Awaiting approval"
        } else {
            "Awaiting input"
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
                    saya_core::contract::ErrorInfo {
                        code: "stale_approval".into(),
                        message: "work mode changed".into(),
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
            self.state.status = "Running".into();
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
            self.notice("The pending question or approval expired.");
            changed = true;
        }
        Ok(changed)
    }
    pub(super) fn show_palette(&mut self) {
        let selected = self.palette.unwrap_or(0);
        self.state.set_details(Details {
            title: "Commands".into(),
            lines: COMMANDS
                .iter()
                .enumerate()
                .map(|(i, command)| {
                    format!(
                        "{} {} · {}",
                        if i == selected { "›" } else { " " },
                        command.text,
                        command.description
                    )
                })
                .chain(std::iter::once("↑/↓ select · Enter run · Esc close".into()))
                .collect(),
        });
        self.state.details_scroll = selected.saturating_sub(8);
    }
    pub(super) async fn submit(&mut self) -> Result<bool> {
        let text = self.state.editor.text().to_owned();
        if text.starts_with('/') && !text.starts_with("//") {
            let exit = self.command(&text).await?;
            self.state.editor.clear();
            return Ok(exit);
        }
        if self.pending.is_some() && self.deferred_mode.is_some() {
            self.notice("Wait for the pending mode change before answering the current request.");
            return Ok(false);
        }
        if matches!(self.pending, Some(Request::Approval(..)))
            && !self
                .state
                .details
                .as_ref()
                .is_some_and(|details| details.title == "Approval required")
        {
            self.show_pending();
            self.notice("Review the current approval details before deciding.");
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
                    let _ = reply.send(Ok(Some(answer)));
                    self.state.editor.clear();
                    self.state.details = None;
                    self.state.status = "Running".into();
                }
                Request::Approval(request, reply) => {
                    match text.trim().to_ascii_lowercase().as_str() {
                        "y" | "yes" => {
                            let _ = reply.send(Ok(Some(true)));
                            self.state.editor.clear();
                            self.state.details = None;
                            self.state.status = "Running".into();
                        }
                        "n" | "no" => {
                            let _ = reply.send(Ok(Some(false)));
                            self.state.editor.clear();
                            self.state.details = None;
                            self.state.status = "Running".into();
                        }
                        _ => {
                            self.pending = Some(Request::Approval(request, reply));
                            self.notice("Approve with y, deny with n, or enter a control command.");
                        }
                    }
                }
                request => self.pending = Some(request),
            }
            return Ok(false);
        }
        if self.active.is_some() {
            self.notice("A run is active; your draft is retained. Ctrl+C cancels.");
            return Ok(false);
        }
        if !text.trim().is_empty() {
            let prompt = if text.starts_with("//") {
                text[1..].to_owned()
            } else {
                text
            };
            self.start(prompt).await?;
            self.state.editor.clear();
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
            "/mode" => {
                let target = mode(argument).ok_or("Use /mode plan|build|code|work")?;
                self.request_mode(target, true).await?;
            }
            "/permissions" => {
                if self.active.is_some() || self.ui_job.is_some() {
                    return Err("Permissions can be changed while idle".into());
                }
                self.config.permissions_mode =
                    permission(argument).ok_or("Use /permissions auto|manual|bypass")?;
                self.refresh_header();
                self.notice("Permissions updated for subsequent tool calls.");
            }
            "/status" => {
                self.refresh_mode()?;
                let session = self
                    .prepared
                    .as_ref()
                    .map(|prepared| prepared.session.id.0.to_string())
                    .unwrap_or_else(|| "No session".into());
                self.notice(&format!(
                    "{} · session {} · mode {} · pending {:?} · permissions {}",
                    self.state.status,
                    session,
                    self.state.mode,
                    self.state.pending_mode,
                    self.state.permissions
                ));
            }
            "/help" => {
                self.state.set_details(Details {
                    title: "Help".into(),
                    lines: COMMANDS
                        .iter()
                        .map(|command| format!("{} · {}", command.text, command.description))
                        .chain([
                            "Enter send · Alt+Enter / Ctrl+J newline".into(),
                            "Empty draft · Tab cycles WORK → CODE → PLAN → BUILD".into(),
                            "Ctrl+P commands · Ctrl+C cancel · Ctrl+D exit".into(),
                            "PgUp/PgDn scroll · End follow output".into(),
                            "Click thinking / long tool headers to expand or collapse".into(),
                            "Ctrl+O toggles the latest foldable card · wheel scrolls the pane under the pointer".into(),
                            "Type / for suggestions · ↑/↓ select · Tab/→ complete · Esc dismiss"
                                .into(),
                        ])
                        .collect(),
                });
                self.state.details_scroll = 0;
            }
            "/new" | "/profile" | "/profiles" | "/resume" | "/resume-accept" | "/sessions"
            | "/tools" | "/jobs" | "/export" | "/export-full" | "/cancel-job" => {
                self.service(name, argument).await?
            }
            _ => return Err("Unknown command. Ctrl+P shows available commands.".into()),
        }
        Ok(false)
    }
}
