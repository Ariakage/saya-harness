//! Interface orchestration using shared application services and durable events.
use crate::{
    terminal::TerminalGuard,
    view::{self, Role, State},
};
use crossterm::event::{
    Event as TerminalEvent, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use futures_util::StreamExt;
use saya_app::{
    events::{ChannelSink, EventModel, EventStore, EventWriter},
    interaction::RunCancellation,
    session::{self, Prepared},
};
use saya_core::{
    approvals::Preauthorizations,
    boundary::Store,
    config::Config,
    contract::{
        BoundaryError, Event, EventPayload, ModelPurpose, RunId, RunResult, Session, WorkMode,
    },
    runtime::PersonalContext,
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
        };
        app.refresh_header();
        app.state.push_message(Role::System,"Saya", "Ready. Enter sends a message; Alt+Enter or Ctrl+J adds a line. Ctrl+C cancels. Ctrl+D exits when idle.");
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
        self.state.mode = format!("{:?}", self.config.mode).to_uppercase();
        self.state.permissions = format!("{:?}", self.config.permissions_mode);
    }
    async fn start(&mut self, text: String) -> Result<()> {
        if self.active.is_some() {
            self.notice("A run is active. Ctrl+C cancels it.");
            return Ok(());
        }
        let display = self.safe(&text);
        if self.prepared.is_none() {
            self.prepared =
                Some(session::create(&self.config, saya_app::new_session(&self.config)?).await?);
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
        let (mut runtime, _) = saya_app::build_runtime(
            &self.config,
            &prepared.session,
            &text,
            saya_app::Launch {
                permissions: self.config.permissions_mode,
                store,
                cancellation: cancel.clone(),
                confirmation: None,
                input: None,
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
        self.state.push_message(Role::User, "You", display);
        let assistant_id = self
            .state
            .push_message(Role::Assistant, self.state.model.clone(), "");
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
        });
        self.state.status = "Running".into();
        self.state.scroll_to_bottom();
        Ok(())
    }
    fn event(&mut self, event: Event) {
        match event.payload {
            EventPayload::ModelTextDelta { text, .. } => {
                let text = self.safe(&text);
                if let Some(active) = &self.active {
                    self.state.append_message(active.assistant_id, &text);
                }
            }
            EventPayload::ModelStarted { purpose, .. } => {
                self.state.status = if purpose == ModelPurpose::Review {
                    "Reviewing"
                } else {
                    "Running"
                }
                .into()
            }
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
        let (outcome, session) = result.map_err(|_| "run task failed")?;
        while let Ok(event) = active.events.try_recv() {
            if let EventPayload::ModelTextDelta { text, .. } = event.payload {
                self.state
                    .append_message(active.assistant_id, &self.safe(&text));
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
        if key.kind == KeyEventKind::Release {
            return Ok(false);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => {
                    self.cancel();
                    return Ok(false);
                }
                KeyCode::Char('d') if self.state.editor.text().is_empty() => {
                    if self.active.is_some() {
                        self.exit_after_run = Some(0);
                        self.cancel();
                        return Ok(false);
                    }
                    return Ok(true);
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::End if self.state.scroll > 0 => self.state.scroll_to_bottom(),
            KeyCode::PageUp => self.state.scroll_up(10),
            KeyCode::PageDown => self.state.scroll_down(10),
            KeyCode::Enter
                if !key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) =>
            {
                let text = self.state.editor.text().to_owned();
                if text.trim() == "/exit" {
                    if self.active.is_some() {
                        self.exit_after_run = Some(0);
                        self.cancel();
                    } else {
                        return Ok(true);
                    }
                } else if self.active.is_some() {
                    self.notice("A run is active. Ctrl+C cancels it; your draft is retained.");
                } else if !text.trim().is_empty() {
                    self.start(text).await?;
                    self.state.editor.clear();
                }
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
        let _ = (&mut active.join).await;
    }
    outcome
}
async fn run_loop(app: &mut App, terminal: &mut TerminalGuard) -> Result<u8> {
    let mut input = EventStream::new();
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
            Finished(std::result::Result<RunOutcome, tokio::task::JoinError>),
        }
        let action = if let Some(active) = &mut app.active {
            tokio::select! {biased;
                _=interrupt.recv()=>Action::Signal,
                _=terminate.recv()=>Action::Signal,
                _=hangup.recv()=>Action::Signal,
                event=input.next(), if input_open=>Action::Input(event),
                _=tick.tick()=>Action::Tick,
                event=active.events.recv(), if active.events_open=>Action::Event(event),
                result=&mut active.join=>Action::Finished(result),
            }
        } else {
            tokio::select! {biased; _=interrupt.recv()=>Action::Signal,_=terminate.recv()=>Action::Signal,
            _=hangup.recv()=>Action::Signal,event=input.next(), if input_open=>Action::Input(event),_=tick.tick()=>Action::Tick}
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
                if app.active.is_some() {
                    app.exit_after_run = Some(1);
                    app.cancel();
                } else {
                    return Ok(1);
                }
            }
            Action::Signal => {
                if app.active.is_some() {
                    app.exit_after_run = Some(130);
                    app.cancel();
                } else {
                    return Ok(130);
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
                if let Some(code) = app.exit_after_run {
                    return Ok(code);
                }
            }
        }
        if redraw && dirty {
            terminal.draw(|frame| view::draw(frame, &mut app.state))?;
            dirty = false;
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
