mod chat;
mod events;
mod export;
mod logs;
mod mcp;
mod signals;
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
    io::{self, IsTerminal, Read, Write},
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
    #[arg(long, global = true, value_parser = ["trace", "debug", "info", "warn", "error"])]
    log_level: Option<String>,
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
    /// Manage explicitly configured MCP connections and credential references.
    Mcp {
        #[command(subcommand)]
        action: mcp::McpAction,
    },
    /// Open the debug-only interactive TUI fixture viewer.
    #[cfg(debug_assertions)]
    TuiPreview {
        #[arg(default_value = "markdown", value_parser = clap::builder::PossibleValuesParser::new(saya_tui::preview::SCENES.iter().copied()))]
        scene: String,
        /// Initially show assistant Markdown source.
        #[arg(long)]
        source: bool,
    },
    Logs {
        #[command(subcommand)]
        action: logs::LogsAction,
    },
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
    Export(export::ExportArgs),
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
    if matches!(output, Some(Output::Json | Output::Jsonl)) {
        let mut value = serde_json::json!({"schema_version":1,"data":null,"error":{"code":"invalid_query","message":message.to_string()}});
        if output == Some(Output::Jsonl) {
            value["type"] = "query.failed".into();
        }
        println!("{value}");
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
            if saya_core::text::is_terminal_control(c) && c != '\n' && c != '\t' {
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
        let prompt = redacted_text(prompt, &self.secrets);
        let mut written = 0;
        while written < prompt.len() {
            if cancel.is_cancelled() {
                return Ok(None);
            }
            match tty.write(&prompt.as_bytes()[written..]) {
                Ok(0) => return Err(cli_error("terminal_error", "controlling terminal closed")),
                Ok(count) => written += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    tokio::select! { biased;
                        _ = cancel.cancelled() => return Ok(None),
                        _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {}
                    }
                }
                Err(_) => {
                    return Err(cli_error(
                        "terminal_error",
                        "cannot write to controlling terminal",
                    ));
                }
            }
        }
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
struct PrintStream {
    inner: Box<dyn ModelStream>,
    // Provider adapters bound per-response text before delivering these updates.
    // Keep the raw displayed prefix separately for each model request.
    displayed: String,
}
impl PrintStream {
    fn write_text(text: &str) -> Result<(), BoundaryError> {
        if text.is_empty() {
            return Ok(());
        }
        let mut stdout = io::stdout().lock();
        stdout
            .write_all(safe_terminal_text(text).as_bytes())
            .and_then(|()| stdout.flush())
            .map_err(|_| cli_error("output_error", "cannot write text output"))
    }
}
impl ModelStream for PrintStream {
    fn next<'a>(&'a mut self, cancel: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
        Box::pin(async move {
            let item = self.inner.next(cancel).await?;
            match &item {
                Some(ModelUpdate::TextDelta(text)) => {
                    Self::write_text(text)?;
                    self.displayed.push_str(text);
                }
                Some(ModelUpdate::Finished { items, .. }) => {
                    // AssistantText is the adapter's validated visible projection.
                    let completed: String = items
                        .iter()
                        .filter_map(|item| {
                            if let HistoryItem::AssistantText(text) = item {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect();
                    if let Some(suffix) = completed.strip_prefix(&self.displayed) {
                        Self::write_text(suffix)?;
                    } else if !completed.is_empty() {
                        // Interleaved output items can arrive in a different order.
                        // Preserve the stream and show the complete validated answer.
                        Self::write_text("\n")?;
                        Self::write_text(&completed)?;
                    }
                }
                _ => {}
            }
            Ok(item)
        })
    }
}
impl Model for PrintModel {
    fn request_counter(&self) -> Option<&dyn saya_core::context_builder::RequestCounter> {
        self.0.request_counter()
    }

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
                Ok(Box::new(PrintStream {
                    inner: stream,
                    displayed: String::new(),
                }) as Box<dyn ModelStream>)
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
        if contains_encoded_secret(&content, secrets) {
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
static LOG_LEVEL: std::sync::OnceLock<saya_core::logging::LogLevel> = std::sync::OnceLock::new();
static DIAGNOSTICS: std::sync::Mutex<Option<saya_core::logging::diagnostics::Diagnostics>> =
    std::sync::Mutex::new(None);
fn install_diagnostics(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let mut current = DIAGNOSTICS.lock().map_err(|_| "diagnostics lock failed")?;
    if current.is_none() {
        let secrets = config
            .secret_env_names()
            .iter()
            .map(|name| config.secret(name).map(str::to_owned))
            .collect::<Result<Vec<_>, _>>()?;
        let diagnostic = saya_core::logging::diagnostics::Diagnostics::start(
            saya_core::logging::JsonLog::new(&config.state_dir, secrets)
                .with_level(LOG_LEVEL.get().copied().unwrap_or_default()),
        )?;
        tracing::dispatcher::set_global_default(diagnostic.dispatch())
            .map_err(|_| "diagnostics subscriber already installed")?;
        *current = Some(diagnostic);
    }
    Ok(())
}
fn build_runtime(
    config: &Config,
    session: &Session,
    summary: &str,
    launch: Launch,
) -> Result<(AgentRuntime, bool), Box<dyn std::error::Error>> {
    install_diagnostics(config)?;
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
            secret_registry: events.as_ref().and_then(|writer| writer.secret_registry()),
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
    let log = saya_core::logging::JsonLog::new(&config.state_dir, secrets)
        .with_level(LOG_LEVEL.get().copied().unwrap_or_default());
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
    let output = if streamed {
        Some("\n".into())
    } else if result.status != RunStatus::NeedsInput
        && let Some(text) = &result.text
    {
        Some(format!("{}\n", safe_output(text, config)))
    } else {
        None
    };
    if let Some(output) = output
        && io::stdout().lock().write_all(output.as_bytes()).is_err()
    {
        eprintln!("saya: cannot write text output");
        return 1;
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
    let _signals = signals::CancellationListener::start(cancel.clone())?;
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
        let writer = events::EventWriter::new(
            prepared.store.clone(),
            session.id,
            run_id,
            secrets,
            Arc::new(events::StdoutSink),
        );
        let writer = if config.mcp.servers.values().any(|server| server.enabled) {
            writer.with_secret_registry(Arc::default())?
        } else {
            writer
        };
        Some(Arc::new(writer))
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
                if let Err(output_error) = events.emit_failure(result).await {
                    report_runtime_error(&output_error, config);
                }
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
    if let SessionAction::Export(args) = action {
        return export::run(config, args).await;
    }
    session::check_roots(config, &config.cwd, config.mode)?;
    let store = session::open(config)?;
    let mut data = match action {
        SessionAction::Export(_) => return Err("invalid export routing".into()),
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
        Output::Text => println!(
            "{}",
            safe_terminal_text(&serde_json::to_string_pretty(&data)?)
        ),
        Output::Json => println!("{}", serde_json::json!({"schema_version":1,"data":data})),
        Output::Jsonl => return Err("session queries support text or JSON output".into()),
    }
    Ok(0)
}
fn tui_error(message: &str) -> u8 {
    use std::os::fd::RawFd;
    let descriptor: RawFd = libc::STDERR_FILENO;
    // SAFETY: fcntl and write operate on the existing stderr descriptor.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 {
        return 1;
    }
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return 1;
    }
    let message = format!("saya: {}\n", safe_terminal_text(message));
    let bytes = message.as_bytes();
    let mut offset = 0;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
    while offset < bytes.len() && std::time::Instant::now() < deadline {
        let written = unsafe {
            libc::write(
                descriptor,
                bytes[offset..].as_ptr().cast(),
                bytes.len() - offset,
            )
        };
        if written > 0 {
            offset += written as usize;
            continue;
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::WouldBlock {
            std::thread::sleep(std::time::Duration::from_millis(5));
        } else if error.kind() != io::ErrorKind::Interrupted {
            break;
        }
    }
    unsafe {
        libc::fcntl(descriptor, libc::F_SETFL, flags);
    }
    1
}
async fn launch_tui(cli: &Cli, overrides: Overrides, environment: Environment) -> u8 {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(_) => return fail("cannot determine current directory"),
    };
    let config = match config::load(overrides, environment, &cwd) {
        Ok(config) => config,
        Err(error) => return fail(error),
    };
    if config.output != Output::Text {
        return fail("Choose an explicit CLI subcommand for JSON or JSONL output");
    }
    if let Err(error) = session::check_roots(&config, &config.cwd, config.mode) {
        return fail(safe_output(&error.to_string(), &config));
    }
    let context = match personal_context(cli, &config) {
        Ok(context) => context,
        Err(error) => return fail(safe_output(&error.to_string(), &config)),
    };
    if let Err(error) = install_diagnostics(&config) {
        return fail(safe_output(&error.to_string(), &config));
    }
    let secrets = match config
        .secret_env_names()
        .iter()
        .map(|name| config.secret(name).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(secrets) => secrets,
        Err(_) => return fail("sensitive settings unavailable"),
    };
    let options = saya_tui::Options {
        personal_context: context,
        anthropic_prompt_cache: cli.anthropic_prompt_cache,
        cwd_explicit: cli.cwd.is_some(),
        profile_explicit: cli.profile.is_some(),
        mode_override: cli.mode.map(Into::into),
    };
    match saya_tui::run(config, options).await {
        Ok(code) => code,
        Err(error) => tui_error(&redacted_text(&error.to_string(), &secrets)),
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    if let Some(code) =
        saya_core::execution::internal_command(std::env::args_os().skip(1).collect()).await
    {
        return std::process::ExitCode::from(u8::try_from(code).unwrap_or(1));
    }
    if saya_core::execution::pin_launcher_image().is_err() {
        return std::process::ExitCode::from(fail("cannot prepare the Saya execution image"));
    }
    let cli = Cli::parse();
    #[cfg(debug_assertions)]
    if let Some(Command::TuiPreview { scene, source }) = &cli.command {
        if matches!(cli.output, Some(OutputOption::Json | OutputOption::Jsonl)) {
            return std::process::ExitCode::from(fail("TUI preview requires text terminal output"));
        }
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return std::process::ExitCode::from(fail(
                "TUI preview requires terminal stdin and stdout",
            ));
        }
        return std::process::ExitCode::from(match saya_tui::preview::run(scene, *source).await {
            Ok(code) => code,
            Err(error) => tui_error(&error.to_string()),
        });
    }
    if cli.command.is_none() {
        if matches!(cli.output, Some(OutputOption::Json | OutputOption::Jsonl)) {
            return std::process::ExitCode::from(fail(
                "Choose an explicit CLI subcommand for JSON or JSONL output",
            ));
        }
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return std::process::ExitCode::from(fail(
                "TUI requires terminal stdin and stdout. Use saya run --stdin or saya chat",
            ));
        }
    }
    let raw_level = cli
        .log_level
        .clone()
        .or_else(|| std::env::var("SAYA_LOG_LEVEL").ok())
        .unwrap_or_else(|| "warn".into());
    let level = match raw_level.parse::<saya_core::logging::LogLevel>() {
        Ok(level) => level,
        Err(_) => {
            return std::process::ExitCode::from(output_error(
                "invalid log level",
                cli.output.map(Into::into),
            ));
        }
    };
    let _ = LOG_LEVEL.set(level);
    let o = overrides(&cli);
    let env = Environment::capture();
    let code = match &cli.command {
        // The preview is dispatched before runtime configuration and environment capture.
        #[cfg(debug_assertions)]
        Some(Command::TuiPreview { .. }) => fail("TUI preview dispatch is unavailable"),
        None => launch_tui(&cli, o, env).await,
        Some(Command::Logs { action }) => match std::env::current_dir()
            .map_err(|_| "cannot determine current directory".to_owned())
            .and_then(|cwd| config::load(o, env, &cwd).map_err(|error| error.to_string()))
        {
            Err(error) => query_error(error, cli.output.map(Into::into)),
            Ok(config) => match logs::run(&config, action).await {
                Ok(code) => code,
                Err(error) => query_error(
                    safe_output(&error.to_string(), &config),
                    Some(config.output),
                ),
            },
        },
        Some(Command::Mcp { action }) => match std::env::current_dir()
            .map_err(|_| "cannot determine current directory".to_owned())
            .and_then(|cwd| config::load(o, env, &cwd).map_err(|error| error.to_string()))
        {
            Err(error) => query_error(error, cli.output.map(Into::into)),
            Ok(config) => mcp::run(&config, action).await,
        },
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
        Some(Command::Chat) => match std::env::current_dir()
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
    if let Ok(mut diagnostics) = DIAGNOSTICS.lock()
        && let Some(diagnostics) = diagnostics.take()
        && diagnostics
            .flush(std::time::Duration::from_secs(2))
            .is_err()
    {
        if cli.command.is_none() {
            tui_error("diagnostic log flush failed");
        } else {
            eprintln!("saya: diagnostic log flush failed");
        }
    }
    std::process::ExitCode::from(code)
}

#[cfg(test)]
mod report_tests {
    use super::*;
    #[tokio::test]
    async fn approval_prompt_backpressure_remains_cancellable() {
        use std::os::fd::{AsRawFd, FromRawFd};
        let mut master = -1;
        let mut slave = -1;
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        let _master = unsafe { std::fs::File::from_raw_fd(master) };
        let mut output = unsafe { std::fs::File::from_raw_fd(slave) };
        let flags = unsafe { libc::fcntl(output.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe { libc::fcntl(output.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );
        loop {
            match output.write(&[b'x'; 8192]) {
                Ok(count) => assert!(count > 0),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => panic!("PTY fixture failed: {error}"),
            }
        }
        let terminal = Terminal {
            file: tokio::sync::Mutex::new(output),
            secrets: Vec::new(),
        };
        let cancel = Arc::new(Cancel::new());
        let signal = cancel.clone();
        let stop = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            signal.cancel();
        });
        let answer = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            terminal.read_line(&"Long approval prompt ".repeat(4096), cancel.as_ref()),
        )
        .await
        .expect("terminal write must yield to cancellation")
        .expect("backpressure is not an I/O failure");
        assert!(answer.is_none());
        stop.await.unwrap();
    }
    #[test]
    fn tui_preview_command_follows_the_build_profile_gate() {
        #[cfg(debug_assertions)]
        {
            let cli = Cli::try_parse_from(["saya", "tui-preview", "markdown", "--source"]).unwrap();
            assert!(
                matches!(cli.command, Some(Command::TuiPreview { scene, source: true }) if scene == "markdown")
            );
            assert!(Cli::try_parse_from(["saya", "tui-preview", "unknown-scene"]).is_err());
        }
        #[cfg(not(debug_assertions))]
        assert!(Cli::try_parse_from(["saya", "tui-preview"]).is_err());
    }
    #[test]
    fn shared_controls_are_replaced_while_unicode_and_layout_remain_readable() {
        let controls: String = (0..=0x1f)
            .chain(0x7f..=0x9f)
            .chain([0x061c, 0x200e, 0x200f])
            .chain(0x202a..=0x202e)
            .chain(0x2066..=0x206f)
            .map(|value| char::from_u32(value).unwrap())
            .filter(|c| !matches!(c, '\n' | '\t'))
            .collect();
        assert_eq!(
            safe_terminal_text(&controls),
            "�".repeat(controls.chars().count())
        );
        let normal = "中文 e\u{301} 👩\u{200d}💻 🌸\n\t";
        assert_eq!(safe_terminal_text(normal), normal);
    }
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
