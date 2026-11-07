mod chat;
mod events;
use saya_app::{new_session, session};

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use saya_core::{
    approvals::Preauthorizations,
    boundary::{Cancellation, Model, ModelRequest, ModelStream, ModelUpdate, Operation, Store},
    config::{self, Config, Environment, Output, Overrides},
    contract::{
        ApprovalRequest, BoundaryError, ErrorInfo, HistoryItem, ModelPurpose, PermissionMode,
        Persistence, RunId, RunResult, RunStatus, SCHEMA_VERSION, Session, WorkMode,
    },
    input::{InputHandler, InputRequest},
    runtime::{AgentRuntime, ApprovalHandler, PersonalContext},
};
use std::{
    fs::OpenOptions,
    io::{self, Read, Write},
    path::PathBuf,
    sync::Arc,
};

#[derive(Parser)]
#[command(name = "saya", version, about = "Saya Agent Runtime Harness")]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[arg(long, global = true)]
    profile: Option<String>,
    #[arg(long, global = true, value_enum)]
    mode: Option<Mode>,
    #[arg(long, global = true, value_enum)]
    permissions: Option<Permissions>,
    #[arg(long, global = true)]
    cwd: Option<PathBuf>,
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[arg(long, global = true, value_enum)]
    output: Option<OutputOption>,
    /// Explicitly provide user preferences for this invocation.
    #[arg(long, global = true)]
    preferences_file: Option<PathBuf>,
    /// Explicitly provide long-term memory for this invocation.
    #[arg(long, global = true)]
    memory_file: Option<PathBuf>,
    /// Enable Anthropic ephemeral prompt caching on endpoints that support it.
    #[arg(long, global = true)]
    anthropic_prompt_cache: bool,
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Clone, Copy, ValueEnum)]
enum Mode {
    Plan,
    Build,
    Code,
    Work,
}
impl From<Mode> for WorkMode {
    fn from(v: Mode) -> Self {
        match v {
            Mode::Plan => Self::Plan,
            Mode::Build => Self::Build,
            Mode::Code => Self::Code,
            Mode::Work => Self::Work,
        }
    }
}
#[derive(Clone, Copy, ValueEnum)]
enum Permissions {
    Auto,
    Bypass,
    Manual,
}
impl From<Permissions> for PermissionMode {
    fn from(v: Permissions) -> Self {
        match v {
            Permissions::Auto => Self::Auto,
            Permissions::Bypass => Self::Bypass,
            Permissions::Manual => Self::Manual,
        }
    }
}
#[derive(Clone, Copy, ValueEnum)]
enum OutputOption {
    Text,
    Json,
    Jsonl,
}
impl From<OutputOption> for Output {
    fn from(v: OutputOption) -> Self {
        match v {
            OutputOption::Text => Self::Text,
            OutputOption::Json => Self::Json,
            OutputOption::Jsonl => Self::Jsonl,
        }
    }
}
#[derive(Subcommand)]
enum Command {
    Run(RunArgs),
    Chat,
    Resume {
        session_id: String,
        #[arg(long)]
        accept_config_change: bool,
    },
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}
#[derive(Subcommand)]
enum SessionAction {
    List {
        #[arg(long, default_value_t = 1)]
        page: u64,
    },
    Show {
        session_id: String,
    },
}
#[derive(Subcommand)]
enum ConfigAction {
    Init,
    Check,
}
#[derive(Args)]
#[command(group(ArgGroup::new("input").required(true).multiple(false).args(["prompt", "prompt_file", "stdin"])))]
struct RunArgs {
    prompt: Option<String>,
    #[arg(long)]
    prompt_file: Option<PathBuf>,
    #[arg(long)]
    stdin: bool,
    #[arg(long)]
    interactive: bool,
    #[arg(long)]
    approval_file: Option<PathBuf>,
    #[arg(long)]
    session: Option<String>,
    #[arg(long, requires = "session")]
    accept_config_change: bool,
}
fn overrides(cli: &Cli) -> Overrides {
    Overrides {
        config: cli.config.clone(),
        profile: cli.profile.clone(),
        mode: cli.mode.map(Into::into),
        permissions: cli.permissions.map(Into::into),
        cwd: cli.cwd.clone(),
        data_dir: cli.data_dir.clone(),
        output: cli.output.map(Into::into),
    }
}
fn fail(message: impl std::fmt::Display) -> u8 {
    eprintln!("saya: {message}");
    2
}
fn output_error(message: impl std::fmt::Display, output: Option<Output>) -> u8 {
    if !matches!(output, Some(Output::Json | Output::Jsonl)) {
        return fail(message);
    }
    let result = RunResult {
        schema_version: SCHEMA_VERSION,
        session_id: None,
        run_id: None,
        status: RunStatus::Failed,
        stop_reason: Some("invalid_input".into()),
        text: None,
        error: Some(ErrorInfo {
            code: "invalid_input".into(),
            message: message.to_string(),
        }),
        usage: None,
        artifacts: vec![],
        persistence: None,
    };
    if output == Some(Output::Jsonl) {
        let timestamp = match events::timestamp() {
            Ok(timestamp) => timestamp,
            Err(error) => return fail(error),
        };
        println!(
            "{}",
            serde_json::json!({"schema_version":1,"session_id":null,"run_id":null,"seq":1,"timestamp":timestamp,"type":"run.finished","data":result})
        );
    } else {
        println!(
            "{}",
            serde_json::to_string(&result).unwrap_or_else(|_| "{}".into())
        );
    }
    2
}
fn query_error(message: impl std::fmt::Display, output: Option<Output>) -> u8 {
    if output == Some(Output::Json) {
        println!(
            "{}",
            serde_json::json!({"schema_version":1,"data":null,"error":{"code":"invalid_query","message":message.to_string()}})
        );
        2
    } else {
        fail(message)
    }
}
fn init(overrides: &Overrides, env: &Environment) -> Result<(), Box<dyn std::error::Error>> {
    let path = config::config_path(overrides, env)?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().write(true).create_new(true).open(path)?;
    f.write_all(b"# Configure an explicit profile before running Saya.\nschema_version = 1\n")?;
    Ok(())
}
fn prompt(args: &RunArgs) -> Result<String, Box<dyn std::error::Error>> {
    let text = if let Some(v) = &args.prompt {
        v.clone()
    } else if let Some(path) = &args.prompt_file {
        std::fs::read_to_string(path)?
    } else {
        let mut v = String::new();
        io::stdin().read_to_string(&mut v)?;
        v
    };
    if text.trim().is_empty() {
        return Err("prompt is empty".into());
    }
    Ok(text)
}
use saya_app::interaction::RunCancellation as Cancel;
struct Terminal {
    file: tokio::sync::Mutex<std::fs::File>,
    secrets: Vec<String>,
}
struct PollGuard(Arc<std::sync::atomic::AtomicBool>);
impl Drop for PollGuard {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }
}
fn safe_terminal_text(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                '\u{fffd}'
            } else {
                c
            }
        })
        .collect()
}
fn redact_value(value: &mut serde_json::Value, secrets: &[String], _depth: usize) {
    if saya_core::redaction::redact(value, secrets).is_err() {
        *value = serde_json::Value::String("sensitive details withheld".into());
    }
}
fn contains_encoded_secret(text: &str, secrets: &[String]) -> bool {
    saya_core::redaction::contains_secret(text, secrets)
}
fn report_runtime_error(error: &BoundaryError, config: &Config) {
    let secrets = config
        .secret_env_names()
        .iter()
        .map(|name| config.secret(name).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>();
    let Ok(secrets) = secrets else {
        eprintln!("saya: run failed; error details unavailable");
        return;
    };
    let mut message = serde_json::Value::String(error.to_string());
    redact_value(&mut message, &secrets, 0);
    let message = message.as_str().unwrap_or("run failed");
    if contains_encoded_secret(message, &secrets) {
        eprintln!("saya: run failed; sensitive error details withheld");
    } else {
        eprintln!("saya: {}", safe_terminal_text(message));
    }
    if let Some(observed) = &error.2 {
        if let Ok(mut value) = serde_json::to_value(observed) {
            redact_value(&mut value, &secrets, 0);
            let rendered = value.to_string();
            if contains_encoded_secret(&rendered, &secrets) {
                eprintln!("saya: observed tool result (not saved): sensitive details withheld");
            } else {
                eprintln!(
                    "saya: observed tool result (not saved): {}",
                    safe_terminal_text(&rendered)
                );
            }
        } else {
            eprintln!("saya: observed tool result could not be displayed");
        }
    }
}
impl Terminal {
    fn open(secrets: Vec<String>) -> io::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open("/dev/tty")?;
        Ok(Self {
            file: tokio::sync::Mutex::new(file),
            secrets,
        })
    }
    async fn read_line(
        &self,
        prompt: &str,
        cancel: &dyn Cancellation,
    ) -> Result<Option<String>, BoundaryError> {
        let mut tty = self.file.lock().await;
        write!(tty, "{}", redacted_text(prompt, &self.secrets))
            .map_err(|_| cli_error("terminal_error", "cannot write to controlling terminal"))?;
        let mut input = tty
            .try_clone()
            .map_err(|_| cli_error("terminal_error", "cannot read controlling terminal"))?;
        let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let _guard = PollGuard(stopped.clone());
        let mut task = tokio::task::spawn_blocking(move || {
            use std::{os::fd::AsRawFd, sync::atomic::Ordering};
            let mut answer = Vec::new();
            while !stopped.load(Ordering::Acquire) {
                let mut descriptor = libc::pollfd {
                    fd: input.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: descriptor is a live pollfd for the owned File; timeout is bounded.
                let ready = unsafe { libc::poll(&mut descriptor, 1, 100) };
                if ready < 0 {
                    return None;
                }
                if ready == 0 {
                    continue;
                }
                let mut byte = [0u8; 1];
                match input.read(&mut byte) {
                    Ok(0) => return None,
                    Ok(_) if byte[0] == b'\n' => {
                        return String::from_utf8(answer)
                            .ok()
                            .map(|v| v.trim_end_matches('\r').to_owned());
                    }
                    Ok(_) if answer.len() < 65536 => answer.push(byte[0]),
                    Ok(_) => return None,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                    Err(_) => return None,
                }
            }
            None
        });
        tokio::select! { biased; () = cancel.cancelled() => Ok(None), result = &mut task => result.map_err(|_| cli_error("terminal_error", "terminal reader failed")) }
    }
}
impl ApprovalHandler for Terminal {
    fn confirm<'a>(
        &'a self,
        request: &'a ApprovalRequest,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, Option<bool>> {
        Box::pin(async move {
            let args = serde_json::to_string(&request.arguments)
                .map_err(|_| cli_error("terminal_error", "cannot format approval"))?;
            let prompt = format!(
                "\nApproval required\nTool: {}\nArguments: {}\nWorking directory: {}\nMode: {:?}\nAllow this call? [y/N] ",
                safe_terminal_text(&request.tool),
                args,
                safe_terminal_text(&request.cwd.to_string_lossy()),
                request.work_mode
            );
            Ok(self
                .read_line(&prompt, cancel)
                .await?
                .map(|answer| matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")))
        })
    }
}
impl InputHandler for Terminal {
    fn ask<'a>(
        &'a self,
        request: &'a InputRequest,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, Option<String>> {
        Box::pin(async move {
            let mut prompt = format!("\n{}\n", safe_terminal_text(&request.question));
            for (index, option) in request.options.iter().enumerate() {
                prompt.push_str(&format!(
                    "  {}. {}\n",
                    index + 1,
                    safe_terminal_text(option)
                ));
            }
            prompt.push_str("Answer: ");
            self.read_line(&prompt, cancel).await
        })
    }
}
struct PrintModel(Arc<dyn Model>);
struct PrintStream(Box<dyn ModelStream>);
impl ModelStream for PrintStream {
    fn next<'a>(&'a mut self, cancel: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
        Box::pin(async move {
            let item = self.0.next(cancel).await?;
            if let Some(ModelUpdate::TextDelta(s)) = &item {
                print!("{s}");
                io::stdout()
                    .flush()
                    .map_err(|_| cli_error("output_error", "cannot write text output"))?;
            }
            Ok(item)
        })
    }
}
impl Model for PrintModel {
    fn validate_session(&self, s: &Session) -> Result<(), BoundaryError> {
        self.0.validate_session(s)
    }
    fn start<'a>(
        &'a self,
        r: &'a ModelRequest,
        c: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            let stream = self.0.start(r, c).await?;
            if r.purpose == ModelPurpose::Execution {
                Ok(Box::new(PrintStream(stream)) as Box<dyn ModelStream>)
            } else {
                Ok(stream)
            }
        })
    }
}
fn cli_error(code: &str, message: &str) -> BoundaryError {
    BoundaryError(
        saya_core::contract::ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}
struct Launch {
    permissions: PermissionMode,
    store: Arc<dyn Store>,
    cancellation: Arc<dyn Cancellation>,
    confirmation: Option<Arc<dyn ApprovalHandler>>,
    input: Option<Arc<dyn InputHandler>>,
    approvals: Preauthorizations,
    personal_context: PersonalContext,
    anthropic_prompt_cache: bool,
    events: Option<Arc<events::EventWriter>>,
    run_id: Option<RunId>,
}
fn personal_context(
    cli: &Cli,
    config: &Config,
) -> Result<PersonalContext, Box<dyn std::error::Error>> {
    fn read(
        path: &Option<PathBuf>,
        secrets: &[String],
    ) -> Result<Option<String>, Box<dyn std::error::Error>> {
        let Some(path) = path else { return Ok(None) };
        use std::os::unix::fs::OpenOptionsExt;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(path)
            .map_err(|_| "cannot open personal context file")?;
        if !file.metadata()?.is_file() {
            return Err("personal context must be a regular file".into());
        }
        let mut bytes = Vec::new();
        file.take(65_537).read_to_end(&mut bytes)?;
        if bytes.len() > 65_536 {
            return Err("personal context file exceeds 64 KiB".into());
        }
        let content = String::from_utf8(bytes).map_err(|_| "personal context must be UTF-8")?;
        if secrets
            .iter()
            .any(|secret| !secret.is_empty() && content.contains(secret))
        {
            return Err("personal context contains a configured secret".into());
        }
        Ok(Some(content))
    }
    let secrets = config
        .secret_env_names()
        .iter()
        .map(|name| config.secret(name).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PersonalContext {
        preferences: read(&cli.preferences_file, &secrets)?,
        memory: read(&cli.memory_file, &secrets)?,
    })
}
fn build_runtime(
    config: &Config,
    session: &Session,
    summary: &str,
    launch: Launch,
) -> Result<(AgentRuntime, bool), Box<dyn std::error::Error>> {
    let events = launch.events;
    saya_app::build_runtime(
        config,
        session,
        summary,
        saya_app::Launch {
            permissions: launch.permissions,
            store: launch.store,
            cancellation: launch.cancellation,
            confirmation: launch.confirmation,
            input: launch.input,
            approvals: launch.approvals,
            personal_context: launch.personal_context,
            anthropic_prompt_cache: launch.anthropic_prompt_cache,
            run_id: launch.run_id,
        },
        move |router, streamed| {
            if let Some(events) = events {
                Arc::new(events::EventModel(router, events))
            } else if streamed {
                Arc::new(PrintModel(router))
            } else {
                router
            }
        },
    )
}
fn redacted_text(text: &str, secrets: &[String]) -> String {
    let mut value = serde_json::Value::String(text.into());
    if saya_core::redaction::redact(&mut value, secrets).is_err() {
        return "sensitive details withheld".into();
    }
    safe_terminal_text(value.as_str().unwrap_or("sensitive details withheld"))
}
fn safe_output(text: &str, config: &Config) -> String {
    let secrets = config
        .secret_env_names()
        .iter()
        .map(|name| config.secret(name).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>();
    let Ok(secrets) = secrets else {
        return "sensitive details withheld".into();
    };
    redacted_text(text, &secrets)
}
fn log_warning(config: &Config, level: &str, event: &str, data: serde_json::Value) {
    let secrets = config
        .secret_env_names()
        .iter()
        .map(|name| config.secret(name).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>();
    let Ok(secrets) = secrets else {
        eprintln!("saya: structured log unavailable");
        return;
    };
    let log = saya_core::logging::JsonLog::new(&config.state_dir, secrets);
    if log.record(level, event, data).is_err() {
        eprintln!("saya: structured log unavailable");
    }
}
fn log_run_result(config: &Config, result: &RunResult) {
    if result.status != RunStatus::Completed {
        log_warning(
            config,
            "warn",
            "run.finished",
            serde_json::json!({"session_id":result.session_id,"run_id":result.run_id,"status":result.status,"stop_reason":result.stop_reason,"error":result.error,"usage":result.usage}),
        );
    }
}
fn report_result(
    result: &saya_core::contract::RunResult,
    session: &Session,
    streamed: bool,
    config: &Config,
) -> u8 {
    if streamed {
        println!();
    } else if result.status != RunStatus::NeedsInput
        && let Some(text) = &result.text
    {
        println!("{}", safe_output(text, config));
    }
    if result.stop_reason.as_deref() == Some("completed_with_background_cleanup") {
        eprintln!("saya: unfinished background jobs were cancelled when the run ended");
    }
    if result.status == RunStatus::NeedsInput
        && let Some(question) = session.history.iter().rev().find_map(|item| match item {
            HistoryItem::ToolResult { result, .. }
                if result
                    .error
                    .as_ref()
                    .is_some_and(|e| e.code == "needs_input") =>
            {
                result.data.as_ref()
            }
            _ => None,
        })
    {
        eprintln!(
            "saya: clarification: {}",
            safe_output(&question.to_string(), config)
        );
    }
    if let Some(e) = &result.error {
        eprintln!(
            "saya: {}: {}",
            safe_output(&e.code, config),
            safe_output(&e.message, config)
        );
    }
    match result.status {
        RunStatus::Completed => 0,
        RunStatus::Failed => 1,
        RunStatus::NeedsConfirmation => 3,
        RunStatus::NeedsInput => 4,
        RunStatus::LimitReached => 5,
        RunStatus::Cancelled => 130,
    }
}
fn status_code(status: RunStatus) -> u8 {
    match status {
        RunStatus::Completed => 0,
        RunStatus::Failed => 1,
        RunStatus::NeedsConfirmation => 3,
        RunStatus::NeedsInput => 4,
        RunStatus::LimitReached => 5,
        RunStatus::Cancelled => 130,
    }
}
fn report_json(result: &RunResult, config: &Config) -> Result<u8, Box<dyn std::error::Error>> {
    let secrets = config
        .secret_env_names()
        .iter()
        .map(|name| config.secret(name).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    let mut value = serde_json::to_value(result)?;
    redact_value(&mut value, &secrets, 0);
    let encoded = serde_json::to_string(&value)?;
    if contains_encoded_secret(&encoded, &secrets) {
        return Err("sensitive output details withheld".into());
    }
    println!("{encoded}");
    Ok(status_code(result.status))
}
async fn run(
    config: &Config,
    args: &RunArgs,
    text: String,
    personal_context: PersonalContext,
    anthropic_prompt_cache: bool,
    resume_options: session::ResumeOptions,
) -> Result<u8, Box<dyn std::error::Error>> {
    let terminal = if args.interactive {
        let secrets = config
            .secret_env_names()
            .iter()
            .map(|name| config.secret(name).map(str::to_owned))
            .collect::<Result<Vec<_>, _>>()?;
        match Terminal::open(secrets) {
            Ok(t) => Some(Arc::new(t)),
            Err(e) => {
                eprintln!("terminal unavailable: {e}");
                None
            }
        }
    } else {
        None
    };
    let mut prepared = if let Some(id) = &args.session {
        session::resume(config, id, resume_options).await?
    } else {
        session::create(config, new_session(config)?).await?
    };
    if prepared.recovery.interrupted_runs > 0 {
        eprintln!(
            "saya: recovered {} interrupted run(s); {} tool outcome(s) unknown; no tools were replayed",
            prepared.recovery.interrupted_runs, prepared.recovery.unknown_tools
        );
    }
    session::append(&mut prepared, text.clone()).await?;
    let mut session = prepared.session.clone();
    let approvals = if let Some(path) = &args.approval_file {
        Preauthorizations::from_file(path, &session.workspace)?
    } else {
        Preauthorizations::default()
    };
    let cancel = Arc::new(Cancel::new());
    let signal = cancel.clone();
    let mut interrupts = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    tokio::spawn(async move {
        if interrupts.recv().await.is_some() {
            signal.cancel();
            if interrupts.recv().await.is_some() {
                std::process::exit(130);
            }
        }
    });
    let confirmation = terminal
        .as_ref()
        .map(|t| t.clone() as Arc<dyn ApprovalHandler>);
    let input = terminal.map(|t| t as Arc<dyn InputHandler>);
    let run_id = RunId::new();
    let events = if config.output == Output::Jsonl {
        let secrets = config
            .secret_env_names()
            .iter()
            .map(|name| config.secret(name).map(str::to_owned))
            .collect::<Result<Vec<_>, _>>()?;
        Some(Arc::new(events::EventWriter::new(
            prepared.store.clone(),
            session.id,
            run_id,
            secrets,
            Arc::new(events::StdoutSink),
        )))
    } else {
        None
    };
    let store: Arc<dyn Store> = if let Some(events) = &events {
        Arc::new(events::EventStore(events.clone()))
    } else {
        prepared.store.clone()
    };
    let (mut runtime, streamed) = build_runtime(
        config,
        &session,
        &text,
        Launch {
            permissions: config.permissions_mode,
            store,
            cancellation: cancel,
            confirmation,
            input,
            approvals,
            personal_context,
            anthropic_prompt_cache,
            events: events.clone(),
            run_id: Some(run_id),
        },
    )?;
    if let Some(events) = &events {
        use saya_core::contract::EventPayload;
        let payload = if args.session.is_some() {
            EventPayload::SessionResumed {
                work_mode: session.work_mode,
                permissions: config.permissions_mode,
                workspace: session.workspace.clone(),
                profile: session.profile.clone(),
            }
        } else {
            EventPayload::SessionCreated {
                work_mode: session.work_mode,
                permissions: config.permissions_mode,
                workspace: session.workspace.clone(),
                profile: session.profile.clone(),
            }
        };
        events.emit(payload, None, true).await?;
    }
    match runtime.run(&mut session).await {
        Ok(result) => {
            log_run_result(config, &result);
            match config.output {
                Output::Jsonl => Ok(status_code(result.status)),
                Output::Json => report_json(&result, config),
                Output::Text => Ok(report_result(&result, &session, streamed, config)),
            }
        }
        Err(error) => {
            log_warning(
                config,
                "error",
                "run.failed",
                serde_json::json!({"session_id":session.id,"run_id":run_id,"error":error.0}),
            );
            report_runtime_error(&error, config);
            if let Some(events) = &events {
                let result = RunResult {
                    schema_version: SCHEMA_VERSION,
                    session_id: Some(session.id),
                    run_id: Some(run_id),
                    status: RunStatus::Failed,
                    stop_reason: Some(error.0.code.clone()),
                    text: None,
                    error: Some(error.0.clone()),
                    usage: None,
                    artifacts: vec![],
                    persistence: Some(Persistence {
                        saved: false,
                        error: Some(error.0.clone()),
                    }),
                };
                events
                    .emit(
                        saya_core::contract::EventPayload::RunFinished(result),
                        Some(run_id),
                        false,
                    )
                    .await?;
                return Ok(1);
            }
            if config.output == Output::Json {
                let result = RunResult {
                    schema_version: SCHEMA_VERSION,
                    session_id: Some(session.id),
                    run_id: Some(run_id),
                    status: RunStatus::Failed,
                    stop_reason: Some(error.0.code.clone()),
                    text: None,
                    error: Some(error.0.clone()),
                    usage: None,
                    artifacts: vec![],
                    persistence: Some(Persistence {
                        saved: false,
                        error: Some(error.0),
                    }),
                };
                return report_json(&result, config);
            }
            Ok(1)
        }
    }
}
async fn query_sessions(
    config: &Config,
    action: &SessionAction,
) -> Result<u8, Box<dyn std::error::Error>> {
    session::check_roots(config, &config.cwd, config.mode)?;
    let store = session::open(config)?;
    let mut data = match action {
        SessionAction::List { page } => {
            let offset = page
                .checked_sub(1)
                .and_then(|v| v.checked_mul(20))
                .ok_or("invalid page")?;
            serde_json::to_value(store.list_sessions(offset, 20).await?)?
        }
        SessionAction::Show { session_id } => {
            let id = session::parse_id(session_id)?;
            let mut session =
                serde_json::to_value(store.load_session(id).await?.ok_or("session not found")?)?;
            session.as_object_mut().ok_or("invalid session")?.insert(
                "background_jobs".into(),
                serde_json::to_value(store.background_jobs(id).await?)?,
            );
            session.as_object_mut().ok_or("invalid session")?.insert(
                "project_context_records".into(),
                serde_json::to_value(store.project_context_records(id).await?)?,
            );
            session.as_object_mut().ok_or("invalid session")?.insert(
                "run_metrics".into(),
                serde_json::to_value(store.run_metrics(id).await?)?,
            );
            session
        }
    };
    let secrets = config
        .secret_env_names()
        .iter()
        .map(|name| config.secret(name).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    saya_core::redaction::redact(&mut data, &secrets)?;
    match config.output {
        Output::Text => println!("{}", serde_json::to_string_pretty(&data)?),
        Output::Json => println!("{}", serde_json::json!({"schema_version":1,"data":data})),
        Output::Jsonl => return Err("session queries support text or JSON output".into()),
    }
    Ok(0)
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let o = overrides(&cli);
    let env = Environment::capture();
    let code = match &cli.command {
        Some(Command::Config {
            action: ConfigAction::Init,
        }) => match init(&o, &env) {
            Ok(()) => {
                println!("Configuration template created.");
                0
            }
            Err(e) => fail(e),
        },
        Some(Command::Config {
            action: ConfigAction::Check,
        }) => match std::env::current_dir()
            .map_err(|_| "cannot determine current directory".to_owned())
            .and_then(|cwd| config::load(o, env, &cwd).map_err(|e| e.to_string()))
        {
            Ok(_) => {
                println!("Configuration is valid.");
                0
            }
            Err(e) => fail(e),
        },
        Some(Command::Run(args)) => match std::env::current_dir()
            .map_err(|_| "cannot determine current directory".to_owned())
            .and_then(|cwd| config::load(o, env, &cwd).map_err(|e| e.to_string()))
        {
            Err(e) => output_error(e, cli.output.map(Into::into)),
            Ok(c) => match prompt(args) {
                Err(e) => output_error(safe_output(&e.to_string(), &c), Some(c.output)),
                Ok(text) => match personal_context(&cli, &c) {
                    Err(e) => output_error(safe_output(&e.to_string(), &c), Some(c.output)),
                    Ok(personal) => {
                        let output = c.output;
                        match run(
                            &c,
                            args,
                            text,
                            personal,
                            cli.anthropic_prompt_cache,
                            session::ResumeOptions {
                                accept_config_change: args.accept_config_change,
                                cwd_explicit: cli.cwd.is_some(),
                                profile_explicit: cli.profile.is_some(),
                                mode_override: cli.mode.map(Into::into),
                            },
                        )
                        .await
                        {
                            Ok(code) => code,
                            Err(e) => {
                                if args.session.is_none()
                                    && session::check_roots(&c, &c.cwd, c.mode).is_ok()
                                {
                                    log_warning(
                                        &c,
                                        "error",
                                        "run.setup_failed",
                                        serde_json::json!({"message":safe_output(&e.to_string(),&c)}),
                                    );
                                }
                                output_error(safe_output(&e.to_string(), &c), Some(output))
                            }
                        }
                    }
                },
            },
        },
        Some(Command::Chat) | None => match std::env::current_dir()
            .map_err(|_| "cannot determine current directory".to_owned())
            .and_then(|cwd| config::load(o, env, &cwd).map_err(|e| e.to_string()))
        {
            Err(e) => fail(e),
            Ok(config) => match personal_context(&cli, &config) {
                Err(e) => fail(safe_output(&e.to_string(), &config)),
                Ok(personal) => {
                    let Ok(secrets) = config
                        .secret_env_names()
                        .iter()
                        .map(|name| config.secret(name).map(str::to_owned))
                        .collect::<Result<Vec<_>, _>>()
                    else {
                        return std::process::ExitCode::from(fail("sensitive details unavailable"));
                    };
                    match chat::chat(config, personal, cli.anthropic_prompt_cache, None).await {
                        Ok(code) => code,
                        Err(e) => fail(redacted_text(&e.to_string(), &secrets)),
                    }
                }
            },
        },
        Some(Command::Session { action }) => match std::env::current_dir()
            .map_err(|_| "cannot determine current directory".to_owned())
            .and_then(|cwd| config::load(o, env, &cwd).map_err(|e| e.to_string()))
        {
            Err(e) => query_error(e, cli.output.map(Into::into)),
            Ok(config) => match query_sessions(&config, action).await {
                Ok(code) => code,
                Err(e) => query_error(safe_output(&e.to_string(), &config), Some(config.output)),
            },
        },
        Some(Command::Resume {
            session_id,
            accept_config_change,
        }) => match std::env::current_dir()
            .map_err(|_| "cannot determine current directory".to_owned())
            .and_then(|cwd| config::load(o, env, &cwd).map_err(|e| e.to_string()))
        {
            Err(e) => fail(e),
            Ok(config) => match personal_context(&cli, &config) {
                Err(e) => fail(safe_output(&e.to_string(), &config)),
                Ok(personal) => match session::resume(
                    &config,
                    session_id,
                    session::ResumeOptions {
                        accept_config_change: *accept_config_change,
                        cwd_explicit: cli.cwd.is_some(),
                        profile_explicit: cli.profile.is_some(),
                        mode_override: cli.mode.map(Into::into),
                    },
                )
                .await
                {
                    Err(e) => fail(safe_output(&e.to_string(), &config)),
                    Ok(prepared) => {
                        let Ok(secrets) = config
                            .secret_env_names()
                            .iter()
                            .map(|name| config.secret(name).map(str::to_owned))
                            .collect::<Result<Vec<_>, _>>()
                        else {
                            return std::process::ExitCode::from(fail(
                                "sensitive details unavailable",
                            ));
                        };
                        match chat::chat(
                            config,
                            personal,
                            cli.anthropic_prompt_cache,
                            Some(prepared),
                        )
                        .await
                        {
                            Ok(code) => code,
                            Err(e) => fail(redacted_text(&e.to_string(), &secrets)),
                        }
                    }
                },
            },
        },
    };
    std::process::ExitCode::from(code)
}

#[cfg(test)]
mod report_tests {
    use super::*;
    #[test]
    fn redaction_detects_nested_encoded_credentials() {
        let secret = r#"fake\"key\\tail"#.to_string();
        let inner = serde_json::json!({"value":secret}).to_string();
        let outer = serde_json::json!({"nested":inner}).to_string();
        let mut value = serde_json::json!({"result":outer});
        redact_value(&mut value, std::slice::from_ref(&secret), 0);
        assert!(!contains_encoded_secret(
            &value.to_string(),
            std::slice::from_ref(&secret)
        ));
        let mut encoded = secret.clone();
        for _ in 0..4 {
            let quoted = serde_json::to_string(&encoded).unwrap();
            encoded = quoted[1..quoted.len() - 1].to_owned();
        }
        assert!(contains_encoded_secret(
            &format!("prefix {encoded} suffix"),
            std::slice::from_ref(&secret)
        ));
    }
}
