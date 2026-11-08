//! Interface orchestration using shared application services and durable events.
#[cfg(test)]
mod control_tests;
mod controls;
#[cfg(test)]
mod message_tests;
mod messages;
#[cfg(test)]
mod palette_tests;
#[cfg(test)]
mod service_tests;
mod services;
use crate::{
    commands::COMMANDS,
    input::TerminalInput,
    terminal::TerminalGuard,
    view::{self, Role, State},
};
use crossterm::event::{
    Event as TerminalEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use saya_app::{
    context::{ContextModel, ContextSnapshot, ContextSources},
    events::{ChannelSink, EventModel, EventStore, EventWriter},
    interaction::{Request, RunCancellation, input_channel},
    session::{self, Prepared},
    thinking::{ThinkingLog, ThinkingModel},
};
use saya_core::{
    approvals::Preauthorizations,
    boundary::Store,
    config::Config,
    contract::{
        BoundaryError, Event, EventPayload, HistoryItem, ModelPurpose, RunId, RunResult, Session,
        Usage, WorkMode,
    },
    runtime::{PersonalContext, RuntimeHandle},
};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{mpsc, watch};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
type RunOutcome = (std::result::Result<RunResult, BoundaryError>, Session);
/// Launch options supplied by the executable, independent of terminal rendering.
pub struct Options {
    pub personal_context: PersonalContext,
    pub anthropic_prompt_cache: bool,
    pub cwd_explicit: bool,
    pub profile_explicit: bool,
    pub mode_override: Option<WorkMode>,
}
struct Active {
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
    prepared: Option<Prepared>,
    active: Option<Active>,
    state: State,
    exit_after_run: Option<u8>,
    pending: Option<Request>,
    ui_job: Option<services::UiJob>,
    deferred_mode: Option<WorkMode>,
    queued_prompt: Option<String>,
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
        let mut app = Self {
            config,
            options,
            secrets,
            prepared: None,
            active: None,
            state: State::default(),
            exit_after_run: None,
            pending: None,
            ui_job: None,
            deferred_mode: None,
            queued_prompt: None,
        };
        app.refresh_header();
        if app.config.profile.is_none() {
            app.notice("Choose a configured model profile before sending a message.");
        }
        Ok(app)
    }
    fn safe(&self, text: &str) -> String {
        let mut value = serde_json::Value::String(text.into());
        if saya_core::redaction::redact(&mut value, &self.secrets).is_err() {
            return "Sensitive details withheld".into();
        }
        value
            .as_str()
            .unwrap_or("Sensitive details withheld")
            .into()
    }
    fn notice(&mut self, text: &str) {
        self.state
            .push_message(Role::System, "Saya", self.safe(text));
    }
    fn refresh_header(&mut self) {
        self.state.workspace = self.safe(&self.config.cwd.to_string_lossy());
        self.state.profile = self.safe(self.config.profile.as_deref().unwrap_or("No profile"));
        self.state.model = self
            .config
            .profile
            .as_ref()
            .and_then(|name| self.config.profiles.get(name))
            .map(|p| p.model.clone())
            .unwrap_or_else(|| "Unconfigured".into());
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
            .map(|text| {
                self.safe(text)
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(80)
                    .collect::<String>()
            })
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "New session".into());
        self.state.mode = format!("{:?}", self.config.mode).to_uppercase();
        self.state.permissions = match self.config.permissions_mode {
            saya_core::contract::PermissionMode::Auto => "auto",
            saya_core::contract::PermissionMode::Manual => "Manual Approval",
            saya_core::contract::PermissionMode::Bypass => "bypass permissions",
        }
        .into();
    }
    fn record_context_input(&mut self, usage: Option<Usage>) {
        self.state.context_input_tokens = usage.as_ref().and_then(|usage| usage.input_tokens);
        self.state.context_estimated = usage.is_some_and(|usage| usage.estimated);
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
        if self.active.is_some() {
            self.notice("A run is active. Ctrl+C cancels it.");
            return Ok(());
        }
        if self.ui_job.is_some() {
            return Err("Wait for the UI operation or cancel it before sending a message".into());
        }
        let display = self.safe(&text);
        if self.prepared.is_none() {
            if self.config.profile.is_none() {
                return Err("Choose a configured profile with /profiles and /profile NAME".into());
            }
            self.service("/new", "").await?;
            self.queued_prompt = Some(text);
            return Ok(());
        }
        let prepared = self.prepared.as_mut().ok_or("session is unavailable")?;
        let run_id = RunId::new();
        let (sender, events) = mpsc::channel(256);
        let (context_sender, context) = watch::channel(None);
        let (thinking_sender, thinking) = watch::channel(ThinkingLog::default());
        let thinking_secrets = self.secrets.clone();
        let context_sources = ContextSources {
            project: true,
            personal: self.options.personal_context.preferences.is_some()
                || self.options.personal_context.memory.is_some(),
        };
        let writer = Arc::new(EventWriter::new(
            prepared.store.clone(),
            prepared.session.id,
            run_id,
            self.secrets.clone(),
            Arc::new(ChannelSink(sender)),
        ));
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
            },
            move |model, _| {
                Arc::new(ContextModel::new(
                    Arc::new(EventModel(
                        Arc::new(ThinkingModel::new(model, thinking_sender, thinking_secrets)),
                        writer,
                    )),
                    context_sender,
                    context_sources,
                ))
            },
        )?;
        session::append(prepared, text.clone()).await.map_err(|_| {
            BoundaryError(
                saya_core::contract::ErrorInfo {
                    code: "storage_error".into(),
                    message: "conversation could not be saved or reloaded".into(),
                },
                None,
                None,
            )
        })?;
        let mut current = prepared.session.clone();
        self.refresh_header();
        self.reset_context();
        self.state.push_message(Role::User, "You", display);
        let handle = runtime.handle();
        let join = tokio::spawn(async move {
            let result = runtime.run(&mut current).await;
            (result, current)
        });
        self.active = Some(Active {
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
        });
        self.state.status = "Running".into();
        self.state.scroll_to_bottom();
        Ok(())
    }
    fn cancel(&mut self) {
        if let Some(job) = &self.ui_job {
            job.cancel.cancel();
        }
        self.deferred_mode = None;
        self.queued_prompt = None;
        if self.active.is_none() {
            self.state.pending_mode = None;
        }
        self.invalidate_pending();
        self.state.command_palette = None;
        if let Some(active) = &self.active {
            active.cancel.cancel();
            self.state.status = "Cancelling".into();
        } else {
            self.state.editor.take();
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
            return Err("run is unavailable".into());
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
        let mut failure = None;
        match result {
            Ok((outcome, session)) => {
                self.config.mode = session.work_mode;
                self.install_todos(saya_core::tools::todo::current_todos(&session.history));
                if let Some(prepared) = &mut self.prepared {
                    prepared.session = session;
                }
                match outcome {
                    Ok(result) => {
                        if let Some(text) = result.text.filter(|text| !text.is_empty()) {
                            let id = self.assistant_message();
                            self.state.replace_message(id, self.safe(&text));
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
                        self.state.status = "Failed".into();
                        self.notice(&error.to_string());
                        if error.0.code == "storage_error" || error.2.is_some() {
                            failure = Some(Box::new(error) as Box<dyn std::error::Error>);
                        }
                    }
                }
            }
            Err(_) => {
                let mut active = self.active.take().ok_or("run is unavailable")?;
                active.cancel.cancel();
                active.events.close();
                active.requests.close();
                let cleanup = active.handle.shutdown_background().await;
                self.close_run_messages(&active);
                self.state.status = "Failed".into();
                self.notice("Run task failed. Inspect stored tool records before retrying.");
                self.refresh_header();
                if let Err(error) = cleanup {
                    self.notice(&format!("Background cleanup failed — {error}"));
                    return Err(Box::new(error));
                }
                return Err("run task failed".into());
            }
        }
        let active = self.active.take().ok_or("run is unavailable")?;
        self.close_run_messages(&active);
        self.refresh_header();
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(())
    }
    fn mouse(&mut self, event: MouseEvent) -> bool {
        if self.state.command_palette.is_some() {
            let selected = self.state.palette_index_at(event.column, event.row);
            return match event.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(selected) = selected {
                        self.state.command_palette = Some(selected);
                        true
                    } else {
                        false
                    }
                }
                MouseEventKind::ScrollUp if selected.is_some() => self.state.palette_scroll(true),
                MouseEventKind::ScrollDown if selected.is_some() => {
                    self.state.palette_scroll(false)
                }
                _ => false,
            };
        }
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.state.toggle_at(event.column, event.row)
            }
            MouseEventKind::ScrollUp => self.state.scroll_at(event.column, event.row, true),
            MouseEventKind::ScrollDown => self.state.scroll_at(event.column, event.row, false),
            _ => false,
        }
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
            self.state.command_palette.is_none(),
        );
    }
    async fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        if key.kind == KeyEventKind::Release
            || (matches!(key.code, KeyCode::Enter | KeyCode::Esc)
                && key.kind != KeyEventKind::Press)
        {
            return Ok(false);
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
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => {
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
                            self.notice(
                                "Clear the current draft before entering command arguments.",
                            );
                        } else {
                            self.state.editor.set_text(command);
                        }
                        self.show_pending();
                        return Ok(false);
                    }
                    let result = self.command(command).await;
                    if self.pending.is_some() {
                        self.show_pending();
                    }
                    return result;
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
                .is_some_and(|details| details.title == "Approval required");
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
        match key.code {
            KeyCode::Esc => {
                self.state.details = None;
                self.show_pending();
            }
            KeyCode::PageUp if self.state.details.is_some() => {
                self.state.details_scroll = self
                    .state
                    .details_scroll
                    .saturating_sub(self.state.details_page_rows)
            }
            KeyCode::PageDown if self.state.details.is_some() => {
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
        return Err("TUI requires text output".into());
    }
    let mut app = App::new(config, options)?;
    let mut terminal = TerminalGuard::enter()?;
    let outcome = run_loop(&mut app, &mut terminal).await;
    let cleanup = app.shutdown().await;
    outcome.and_then(|code| cleanup.map(|()| code))
}
async fn run_loop(app: &mut App, terminal: &mut TerminalGuard) -> Result<u8> {
    let mut input = TerminalInput::new()?;
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(33));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    let mut dirty = true;
    let mut input_open = true;
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
                Err(error) => {
                    if error.downcast_ref::<BoundaryError>().is_some_and(|error| {
                        error.0.code.starts_with("storage_") || error.2.is_some()
                    }) {
                        return Err(error);
                    }
                    app.notice(&error.to_string());
                }
            },
            Action::Input(Some(Ok(TerminalEvent::Mouse(event)))) => {
                app.mouse(event);
            }
            Action::Input(Some(Ok(TerminalEvent::Paste(text)))) => {
                if app.state.command_palette.is_none() {
                    app.state.editor.insert(&text);
                }
            }
            Action::Input(Some(Ok(_))) | Action::Tick => {}
            Action::Input(Some(Err(_))) | Action::Input(None) => {
                input_open = false;
                if app.active.is_some() || app.ui_job.is_some() {
                    app.exit_after_run = Some(1);
                    app.cancel();
                } else {
                    return Ok(1);
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
                app.finish_ui(result).await?;
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
        if redraw && dirty {
            terminal.set_title(&format!("Saya | {}", app.state.session_title))?;
            terminal.draw(|frame| view::draw(frame, &mut app.state))?;
            dirty = false;
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
