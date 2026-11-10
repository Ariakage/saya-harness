//! User controls and pending human input. Authorization remains in core.
use super::*;
use crate::view::{Details, MarkdownDisplay};
use saya_app::interaction::Request;
use saya_core::{
    boundary::StoreChange, config::ThinkingEffort, contract::PermissionMode, execution::BackendKind,
};
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
pub(super) fn closed(request: &Request) -> bool {
    match request {
        Request::Approval(_, reply) => reply.is_closed(),
        Request::Input(_, reply) => reply.is_closed(),
    }
}
impl App {
    pub(super) fn require_idle_configuration(&self) -> Result<()> {
        if self.active.is_some() || self.ui_job.is_some() {
            return Err("Runtime settings can be changed while idle".into());
        }
        if self.pending.is_some() {
            return Err("Answer or dismiss the current Agent request first".into());
        }
        Ok(())
    }
    async fn change_workspace(&mut self, argument: &str) -> Result<()> {
        self.require_idle_configuration()?;
        if argument.is_empty() {
            self.notice(&format!(
                "Workspace — {}",
                self.config.cwd.to_string_lossy()
            ));
            return Ok(());
        }
        let requested = Path::new(argument);
        let path = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            self.config.cwd.join(requested)
        };
        let workspace = std::fs::canonicalize(path)
            .map_err(|_| "Workspace directory does not exist or is unavailable")?;
        if !workspace.is_dir() {
            return Err("Workspace must be a directory".into());
        }
        session::check_roots(&self.config, &workspace, self.current_mode()?)?;
        let previous = self.config.cwd.clone();
        self.config.cwd = workspace.clone();
        if let Some(prepared) = &mut self.prepared {
            let mut updated = prepared.session.clone();
            updated.workspace = workspace;
            let fingerprint = self
                .config
                .profiles
                .get(&updated.profile)
                .ok_or("session profile not found")?
                .fingerprint()?;
            if let Err(error) = prepared
                .store
                .update_session_config(&updated, &fingerprint)
                .await
            {
                self.config.cwd = previous;
                return Err(Box::new(error));
            }
            prepared.session = updated;
        }
        self.state.details = None;
        self.state.details_scroll = 0;
        self.state.skills_count = None;
        self.reset_context();
        self.refresh_header();
        self.notice("Workspace changed for subsequent runs in this session.");
        Ok(())
    }
    async fn select_effort(&mut self, argument: &str) -> Result<()> {
        self.require_idle_configuration()?;
        let name = self
            .config
            .profile
            .clone()
            .ok_or("Choose a configured model profile first")?;
        let current = self
            .config
            .profiles
            .get(&name)
            .and_then(|profile| profile.thinking_effort)
            .map_or("default", ThinkingEffort::as_str);
        if argument.is_empty() {
            self.notice(&format!(
                "Thinking effort — {current} · use low|medium|high|xhigh|max|default"
            ));
            return Ok(());
        }
        let selected = effort(argument).ok_or("Use /effort low|medium|high|xhigh|max|default")?;
        let previous = self
            .config
            .profiles
            .get(&name)
            .ok_or("Selected profile is unavailable")?
            .thinking_effort;
        let fingerprint = {
            let profile = self
                .config
                .profiles
                .get_mut(&name)
                .ok_or("Selected profile is unavailable")?;
            profile.thinking_effort = selected;
            profile.fingerprint()?
        };
        if let Some(prepared) = &self.prepared
            && let Err(error) = prepared
                .store
                .update_session_config(&prepared.session, &fingerprint)
                .await
        {
            if let Some(profile) = self.config.profiles.get_mut(&name) {
                profile.thinking_effort = previous;
            }
            return Err(Box::new(error));
        }
        self.refresh_header();
        let value = selected.map_or("default", ThinkingEffort::as_str);
        self.notice(&format!(
            "Thinking effort set to {value} for subsequent requests."
        ));
        Ok(())
    }
    fn set_sandbox(&mut self, argument: &str) -> Result<()> {
        self.require_idle_configuration()?;
        match argument.to_ascii_lowercase().as_str() {
            "" | "status" => {
                self.notice(&format!(
                    "Sandbox — {}",
                    sandbox_label(self.config.sandbox.backend)
                ));
            }
            "on" => {
                self.config.sandbox.backend = BackendKind::Auto;
                self.notice("Sandbox enabled for subsequent runs: platform native backend.");
            }
            "off" => {
                self.config.sandbox.backend = BackendKind::Host;
                self.notice(
                    "Sandbox disabled for subsequent runs. Host execution has no filesystem, network, or syscall isolation.",
                );
            }
            _ => return Err("Use /sandbox on|off|status".into()),
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
        let prompt_title = match &self.pending {
            Some(Request::Approval(..)) => Some("Approval required"),
            Some(Request::Input(..)) => Some("Question"),
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
            self.notice("Review the current request details before replying.");
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
            "/compact" => {
                if !argument.is_empty() {
                    return Err("Use /compact without arguments".into());
                }
                self.compact().await?;
            }
            "/memory" => self.open_memory(argument).await?,
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
            "/cd" => self.change_workspace(argument).await?,
            "/sandbox" => self.set_sandbox(argument)?,
            "/effort" | "/thinking" => self.select_effort(argument).await?,
            "/skills" | "/mcps" | "/plugins" => {
                if !argument.is_empty() {
                    return Err(format!("Use {name} without arguments").into());
                }
                self.open_picker(&name[1..]).await?
            }
            "/markdown" => {
                let display = match argument {
                    "source" => MarkdownDisplay::Source,
                    "render" => MarkdownDisplay::Render,
                    _ => return Err("Use /markdown source|render".into()),
                };
                self.state.set_markdown_display(display);
                self.notice(&format!("Markdown display — {}", display.as_str()));
            }
            "/footnote" => {
                let jumped = if argument == "back" {
                    self.state.footnote_back()
                } else {
                    let number = argument
                        .parse::<usize>()
                        .ok()
                        .filter(|number| *number > 0)
                        .ok_or("Use /footnote NUMBER or /footnote back")?;
                    self.state.jump_footnote(number)
                };
                if !jumped {
                    return Err(
                        "Footnote target is unavailable in the retained rendered transcript".into(),
                    );
                }
                self.state.details = None;
                self.state.details_scroll = 0;
            }
            "/status" => {
                self.refresh_mode()?;
                let session = self
                    .prepared
                    .as_ref()
                    .map(|prepared| prepared.session.id.0.to_string())
                    .unwrap_or_else(|| "No session".into());
                self.notice(&format!(
                    "{} · session {} · mode {} · pending {:?} · permissions {} · sandbox {} · Markdown {}",
                    self.state.status,
                    session,
                    self.state.mode,
                    self.state.pending_mode,
                    self.state.permissions,
                    sandbox_label(self.config.sandbox.backend),
                    self.state.markdown_display.as_str()
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
                            "Click a footnote number to jump to its definition; click its definition marker to return".into(),
                            "/footnote NUMBER jumps in the newest matching assistant message; /footnote back returns".into(),
                            "/markdown source|render selects the assistant presentation".into(),
                            "/cd PATH changes this session workspace; /sandbox on|off|status controls native isolation".into(),
                            "/effort|/thinking LEVEL changes reasoning effort; /model PROFILE changes model profile".into(),
                            "/skills, /mcps and /plugins open selectors · ↑↓ move · Enter selects · Esc closes".into(),
                            "/compact summarizes saved history, or queues at the next model boundary during a run".into(),
                            "/memory opens current memory, source/scope, review, forgetting, history and backup controls".into(),
                            "Type / for suggestions · ↑/↓ select · Tab/→ complete · Esc dismiss"
                                .into(),
                        ])
                        .collect(),
                });
                self.state.details_scroll = 0;
            }
            "/new" | "/profile" | "/model" | "/profiles" | "/resume" | "/resume-accept"
            | "/sessions" | "/tools" | "/jobs" | "/export" | "/export-full" | "/cancel-job" => {
                self.service(name, argument).await?
            }
            _ => return Err("Unknown command. Ctrl+P shows available commands.".into()),
        }
        Ok(false)
    }
}

fn sandbox_label(backend: BackendKind) -> &'static str {
    match backend {
        BackendKind::Auto => "on (native at run start)",
        BackendKind::Seatbelt => "on (Seatbelt)",
        BackendKind::Bubblewrap => "on (Bubblewrap)",
        BackendKind::Host => "off (host compatibility)",
    }
}
