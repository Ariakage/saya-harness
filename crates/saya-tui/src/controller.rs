//! Interface orchestration using shared application services and durable events.
#[cfg(test)]
mod control_tests;
mod controls;
#[cfg(test)]
mod service_tests;
mod services;
use crate::{
    commands::COMMANDS,
    input::TerminalInput,
    terminal::TerminalGuard,
    view::{self, Role, State},
};
use crossterm::event::{Event as TerminalEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use saya_app::{
    events::{ChannelSink, EventModel, EventStore, EventWriter},
    interaction::{Request, RunCancellation, input_channel},
    session::{self, Prepared},
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
use std::sync::Arc;
use tokio::sync::mpsc;

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
    assistant_id: u64,
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
    palette: Option<usize>,
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
            palette: None,
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
            move |model, _| Arc::new(EventModel(model, writer)),
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
        self.record_context_input(None);
        self.state.push_message(Role::User, "You", display);
        let assistant_id = self
            .state
            .push_message(Role::Assistant, self.state.model.clone(), "");
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
            assistant_id,
            handle,
            requests,
            requests_open: true,
        });
        self.state.status = "Running".into();
        self.state.scroll_to_bottom();
        Ok(())
    }
    fn event(&mut self, event: Event) {
        if matches!(
            &event.payload,
            EventPayload::ModelStarted {
                purpose: ModelPurpose::Execution,
                ..
            }
        ) {
            self.record_context_input(None);
        }
        match event.payload {
            EventPayload::ModelTextDelta { text, .. } => {
                let text = self.safe(&text);
                if let Some(active) = &self.active {
                    self.state.append_message(active.assistant_id, &text);
                }
            }
            EventPayload::ModelStarted { purpose, .. } if self.pending.is_none() => {
                if purpose == ModelPurpose::Execution
                    && let Some(active) = &mut self.active
                    && self.state.messages.back().is_none_or(|message| {
                        message.id != active.assistant_id || !message.text.is_empty()
                    })
                {
                    self.state.finish_message(active.assistant_id);
                    active.assistant_id =
                        self.state
                            .push_message(Role::Assistant, self.state.model.clone(), "");
                }
                self.state.status = if purpose == ModelPurpose::Review {
                    "Reviewing"
                } else {
                    "Running"
                }
                .into();
            }
            EventPayload::ModelFinished {
                purpose: ModelPurpose::Execution,
                usage,
                ..
            } => self.record_context_input(usage),
            EventPayload::ToolStarted { name, call_id } => {
                self.state
                    .push_message(Role::Tool, name, format!("Running {call_id}"));
            }
            EventPayload::ToolFinished { name, result, .. } => {
                self.state.push_message(
                    Role::Tool,
                    name,
                    self.safe(&serde_json::to_string(&result).unwrap_or_default()),
                );
            }
            _ => {}
        };
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
        self.palette = None;
        if let Some(active) = &self.active {
            active.cancel.cancel();
            self.state.status = "Cancelling".into();
        } else {
            self.state.editor.take();
        }
    }
    fn finish(
        &mut self,
        result: std::result::Result<RunOutcome, tokio::task::JoinError>,
    ) -> Result<()> {
        let mut active = self.active.take().ok_or("run is unavailable")?;
        self.invalidate_pending();
        self.state.pending_mode = None;
        let (outcome, session) = result.map_err(|_| "run task failed")?;
        while let Ok(event) = active.events.try_recv() {
            match event.payload {
                EventPayload::ModelTextDelta { text, .. } => {
                    self.state
                        .append_message(active.assistant_id, &self.safe(&text));
                }
                EventPayload::ModelFinished {
                    purpose: ModelPurpose::Execution,
                    usage,
                    ..
                } => self.record_context_input(usage),
                EventPayload::ModelStarted {
                    purpose: ModelPurpose::Execution,
                    ..
                } => self.record_context_input(None),
                _ => {}
            }
        }
        self.config.mode = session.work_mode;
        if let Some(prepared) = &mut self.prepared {
            prepared.session = session;
        }
        match outcome {
            Ok(result) => {
                if let Some(text) = &result.text {
                    self.state
                        .replace_message(active.assistant_id, self.safe(text));
                }
                self.state.status = format!("{:?}", result.status);
                if let Some(error) = result.error {
                    self.notice(&format!("{} — {}", error.code, error.message));
                }
                self.state.usage = result
                    .usage
                    .map(|usage| self.safe(&serde_json::to_string(&usage).unwrap_or_default()))
                    .unwrap_or_default();
            }
            Err(error) => {
                self.state.status = "Failed".into();
                self.notice(&error.to_string());
                if error.0.code == "storage_error" || error.2.is_some() {
                    return Err(Box::new(error));
                }
            }
        }
        self.state.finish_message(active.assistant_id);
        self.refresh_header();
        Ok(())
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
            self.palette.is_none(),
        );
    }
    async fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        if key.kind == KeyEventKind::Release {
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
                KeyCode::Char('p') => {
                    self.palette = if self.palette.is_some() {
                        None
                    } else {
                        Some(0)
                    };
                    if self.palette.is_some() {
                        self.show_palette();
                    } else {
                        self.state.details = None;
                        self.show_pending();
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
        if let Some(selected) = self.palette {
            match key.code {
                KeyCode::Up => {
                    self.palette = Some(selected.saturating_sub(1));
                    self.show_palette();
                    return Ok(false);
                }
                KeyCode::Down => {
                    self.palette = Some((selected + 1).min(COMMANDS.len() - 1));
                    self.show_palette();
                    return Ok(false);
                }
                KeyCode::Esc => {
                    self.palette = None;
                    self.state.details = None;
                    self.show_pending();
                    return Ok(false);
                }
                KeyCode::Enter => {
                    let typed = self.state.editor.text().to_owned();
                    let command = if typed.starts_with('/') {
                        typed.as_str()
                    } else {
                        COMMANDS[selected].text
                    };
                    self.palette = None;
                    if !typed.starts_with('/') && command.ends_with(' ') {
                        if !typed.is_empty() {
                            self.notice(
                                "Clear the current draft before entering command arguments.",
                            );
                            self.state.details = None;
                            self.show_pending();
                            return Ok(false);
                        }
                        self.state.editor.set_text(command);
                        self.state.details = None;
                        self.show_pending();
                        return Ok(false);
                    }
                    let exit = self.command(command).await?;
                    if typed.starts_with('/') {
                        self.state.editor.clear();
                    }
                    if self.pending.is_some() {
                        self.show_pending();
                    }
                    return Ok(exit);
                }
                _ => {}
            }
        }
        if key.code == KeyCode::Esc
            && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
            && self.state.completion.visible()
        {
            self.state.completion.dismiss();
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
        let approval_visible = self.palette.is_none()
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
                self.state.details_scroll = self.state.details_scroll.saturating_sub(10)
            }
            KeyCode::PageDown if self.state.details.is_some() => {
                self.state.details_scroll = self.state.details_scroll.saturating_add(10)
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
    if let Some(mut active) = app.active.take() {
        active.cancel.cancel();
        active.events.close();
        active.requests.close();
        let _ = (&mut active.join).await;
    }
    if let Some(mut job) = app.ui_job.take() {
        job.cancel.cancel();
        let _ = (&mut job.join).await;
    }
    outcome
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
            Action::Input(Some(Ok(TerminalEvent::Paste(text)))) => {
                app.state.editor.insert(&text);
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
                app.finish(result)?;
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
