mod approval;
mod chat;
mod events;
mod export;
mod goal_commands;
mod helper_commands;
mod logs;
mod mcp;
mod memory_commands;
mod module_commands;
mod plugin_commands;
mod signals;
mod terminal;
use soyo_app::{new_session, session};
use terminal::{
    PrintModel, Terminal, contains_encoded_secret, redact_value, redacted_text,
    report_runtime_error, safe_output, safe_terminal_text, tui_error,
};

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use soyo_core::{
    approvals::Preauthorizations,
    boundary::{Cancellation, Operation, Store},
    config::{self, Config, Environment, Output, Overrides},
    contract::{
        ApprovalRequest, BoundaryError, ErrorInfo, Event, EventPayload, HistoryItem,
        PermissionMode, Persistence, RunId, RunResult, RunStatus, SCHEMA_VERSION, Session,
        SessionId, WorkMode,
    },
    input::{InputHandler, InputRequest},
    runtime::{AgentRuntime, ApprovalHandler, PersonalContext},
};
use soyo_ipc::{
    client::HelperClient,
    types::{
        ApprovalRequestDto, DesktopError, DesktopStreamEnvelope, PersonalContextDto,
        RunPromptParams, SubmitPromptResultDto,
    },
};
use std::{
    fs::OpenOptions,
    io::{self, IsTerminal, Read, Write},
    path::PathBuf,
    sync::Arc,
};
use tokio::sync::mpsc;

#[derive(Parser)]
#[command(name = "soyo", version, about = "Soyo Agent Runtime Harness")]
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
    /// Plan and continue durable Goals with acceptance evidence and bounded budgets.
    Goal {
        #[command(subcommand)]
        action: goal_commands::GoalAction,
    },
    /// Inspect and administer the configured managed global memory.
    Memory {
        #[command(subcommand)]
        action: memory_commands::MemoryAction,
    },
    /// Inspect and manage local, explicitly enabled plugin packages.
    Plugin {
        #[command(subcommand)]
        action: plugin_commands::PluginAction,
    },
    /// Manage explicitly configured MCP connections and credential references.
    Mcp {
        #[command(subcommand)]
        action: mcp::McpAction,
    },
    /// Manage runtime capability module switches (`browser`, `vm`).
    Module(module_commands::ModuleArgs),
    /// Open the debug-only interactive TUI fixture viewer.
    #[cfg(debug_assertions)]
    TuiPreview {
        #[arg(default_value = "markdown", value_parser = clap::builder::PossibleValuesParser::new(soyo_tui::preview::SCENES.iter().copied()))]
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
    /// Compact saved history without executing tools or adding a user message.
    Compact {
        session_id: String,
        #[arg(long)]
        accept_config_change: bool,
    },
    Chat,
    /// Resume a saved session in the text chat.
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
    /// Run the shared backend daemon used by CLI, TUI, and desktop clients.
    Helper,
    /// Stop the running helper daemon.
    Kill,
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
    eprintln!("soyo: {message}");
    2
}
/// B-17: exit code 2 is reserved for invalid CLI input or configuration;
/// storage and other runtime failures before a run starts exit 1. Status-like
/// codes keep the same exit codes used by the run and MCP result paths.
fn failure_exit_code(code: &str) -> u8 {
    match code {
        "cancelled" => 130,
        "needs_input" | "needs_auth" => 4,
        "needs_confirmation" => 3,
        "limit_reached" | "credential_limit" => 5,
        // `invalid_*` codes that describe malformed model, provider, tool or
        // persisted data are runtime failures, not invalid CLI input.
        "invalid_model_response"
        | "invalid_provider_result"
        | "invalid_response"
        | "invalid_search_response"
        | "invalid_redirect"
        | "invalid_html"
        | "invalid_worker_args"
        | "invalid_worker_request"
        | "invalid_history"
        | "invalid_tool"
        | "invalid_tool_registry"
        | "invalid_tool_schema" => 1,
        "credential_scope" | "plan_write_denied" => 2,
        _ if code.starts_with("invalid_") => 2,
        _ => 1,
    }
}
/// Preserve the error's stable code while redacting configured secrets from
/// its message.
fn sanitized_error(error: &BoundaryError, config: &Config) -> BoundaryError {
    cli_error(&error.0.code, &safe_output(&error.0.message, config))
}
/// Recover the original boundary error carried inside a boxed CLI failure.
/// Plain I/O or string errors keep the generic CLI input/query code.
fn boxed_error(
    error: &(dyn std::error::Error + 'static),
    fallback: &str,
    config: &Config,
) -> BoundaryError {
    match error.downcast_ref::<BoundaryError>() {
        Some(error) => sanitized_error(error, config),
        None => cli_error(fallback, &safe_output(&error.to_string(), config)),
    }
}
fn output_error(error: BoundaryError, output: Option<Output>) -> u8 {
    if !matches!(output, Some(Output::Json | Output::Jsonl)) {
        return fail(&error);
    }
    let exit = failure_exit_code(&error.0.code);
    let result = RunResult {
        schema_version: SCHEMA_VERSION,
        session_id: None,
        run_id: None,
        status: RunStatus::Failed,
        stop_reason: Some(error.0.code.clone()),
        text: None,
        error: Some(error.0),
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
    exit
}
fn query_error(error: BoundaryError, output: Option<Output>) -> u8 {
    if matches!(output, Some(Output::Json | Output::Jsonl)) {
        let exit = failure_exit_code(&error.0.code);
        let mut value = serde_json::json!({"schema_version":1,"data":null,"error":error.0});
        if output == Some(Output::Jsonl) {
            value["type"] = "query.failed".into();
        }
        println!("{value}");
        exit
    } else {
        fail(&error)
    }
}

fn memory_failure_value(
    action: &memory_commands::MemoryAction,
    error: BoundaryError,
) -> serde_json::Value {
    serde_json::json!({"schema_version":1,"kind":"memory","action":action.name(),"ok":false,"data":null,"error":error.0,"actual_result":error.2})
}
pub(crate) fn config_secrets(config: &Config) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    Ok(config
        .secret_env_names()
        .iter()
        .map(|name| config.secret(name).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?)
}
fn load_config(overrides: Overrides, environment: Environment) -> Result<Config, BoundaryError> {
    let cwd = std::env::current_dir()
        .map_err(|_| cli_error("invalid_workspace", "cannot determine current directory"))?;
    config::load(overrides, environment, &cwd)
}
fn failed_run_result(session_id: SessionId, run_id: RunId, error: ErrorInfo) -> RunResult {
    RunResult {
        schema_version: SCHEMA_VERSION,
        session_id: Some(session_id),
        run_id: Some(run_id),
        status: RunStatus::Failed,
        stop_reason: Some(error.code.clone()),
        text: None,
        error: Some(error.clone()),
        usage: None,
        artifacts: vec![],
        persistence: Some(Persistence {
            saved: false,
            error: Some(error),
        }),
    }
}
async fn administer_memory(action: &memory_commands::MemoryAction, config: &Config) -> u8 {
    use std::sync::atomic::{AtomicBool, Ordering};
    let Ok(secrets) = config_secrets(config) else {
        return query_error(
            cli_error("invalid_config", "memory credentials are unavailable"),
            Some(config.output),
        );
    };
    let mut signals = match signals::Signals::new() {
        Ok(signals) => signals,
        Err(_) => {
            return query_error(
                cli_error(
                    "runtime_error",
                    "cannot listen for memory administration cancellation",
                ),
                Some(config.output),
            );
        }
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = cancel.clone();
    let worker_config = config.clone();
    let worker_action = action.clone();
    let mut worker = tokio::task::spawn_blocking(move || {
        memory_commands::run_cancellable(&worker_action, &worker_config, &worker_cancel)
    });
    let (outcome, interrupted) = tokio::select! {
        result = &mut worker => (result,None),
        signal = signals.recv() => {
            if signal.is_some() { cancel.store(true,Ordering::Release); }
            (worker.await,signal)
        },
    };
    let (mut value, status) = match outcome {
        Ok(Ok(value)) => (value, 0),
        Ok(Err(error)) => (memory_failure_value(action, error), 2),
        Err(_) => {
            return query_error(
                cli_error("runtime_error", "memory administration worker failed"),
                Some(config.output),
            );
        }
    };
    let status = match interrupted {
        Some(signals::Received::Interrupt) => 130,
        Some(signals::Received::Terminate) => 143,
        None => status,
    };
    if interrupted.is_some() {
        value["interrupted"] = true.into();
    }
    if soyo_core::redaction::redact(&mut value, &secrets).is_err() {
        return query_error(
            cli_error(
                "runtime_error",
                "memory result contains unavailable sensitive details",
            ),
            Some(config.output),
        );
    }
    if config.output == Output::Jsonl {
        value["type"] = "memory.result".into();
    }
    match writeln!(io::stdout().lock(), "{value}") {
        Ok(()) => status,
        Err(_) => query_error(
            cli_error("runtime_error", "cannot write memory result"),
            Some(config.output),
        ),
    }
}
fn init(overrides: &Overrides, env: &Environment) -> Result<(), Box<dyn std::error::Error>> {
    let path = config::config_path(overrides, env)?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().write(true).create_new(true).open(path)?;
    f.write_all(b"# Configure an explicit profile before running Soyo.\nschema_version = 1\n")?;
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
use soyo_app::interaction::RunCancellation as Cancel;
pub(crate) fn cli_error(code: &str, message: &str) -> BoundaryError {
    BoundaryError(
        soyo_core::contract::ErrorInfo {
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
        if terminal::contains_encoded_secret(&content, secrets) {
            return Err("personal context contains a configured secret".into());
        }
        Ok(Some(content))
    }
    let secrets = config_secrets(config)?;
    Ok(PersonalContext {
        preferences: read(&cli.preferences_file, &secrets)?,
        memory: read(&cli.memory_file, &secrets)?,
    })
}
static LOG_LEVEL: std::sync::OnceLock<soyo_core::logging::LogLevel> = std::sync::OnceLock::new();
static DIAGNOSTICS: std::sync::Mutex<Option<soyo_core::logging::diagnostics::Diagnostics>> =
    std::sync::Mutex::new(None);
fn install_diagnostics(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let mut current = DIAGNOSTICS.lock().map_err(|_| "diagnostics lock failed")?;
    if current.is_none() {
        let secrets = config_secrets(config)?;
        let diagnostic = soyo_core::logging::diagnostics::Diagnostics::start(
            soyo_core::logging::JsonLog::new(&config.state_dir, secrets)
                .with_level(LOG_LEVEL.get().copied().unwrap_or_default()),
        )?;
        tracing::dispatcher::set_global_default(diagnostic.dispatch())
            .map_err(|_| "diagnostics subscriber already installed")?;
        *current = Some(diagnostic);
    }
    Ok(())
}
async fn compact_session(
    cli: &Cli,
    config: &Config,
    session_id: &str,
    accept_config_change: bool,
) -> Result<u8, Box<dyn std::error::Error>> {
    let mut prepared = session::resume(
        config,
        session_id,
        session::ResumeOptions {
            accept_config_change,
            cwd_explicit: cli.cwd.is_some(),
            profile_explicit: cli.profile.is_some(),
            mode_override: cli.mode.map(Into::into),
        },
    )
    .await?;
    let cancel = Arc::new(Cancel::new());
    let _signals = signals::CancellationListener::start(cancel.clone())?;
    let run_id = RunId::new();
    let events = if config.output == Output::Jsonl {
        let secrets = config_secrets(config)?;
        Some(Arc::new(
            events::EventWriter::new(
                prepared.store.clone(),
                prepared.session.id,
                run_id,
                secrets,
                Arc::new(events::StdoutSink),
            )
            .with_secret_registry(Arc::default())?,
        ))
    } else {
        None
    };
    let store: Arc<dyn Store> = match &events {
        Some(events) => Arc::new(events::EventStore(events.clone())),
        None => prepared.store.clone(),
    };
    let (mut runtime, _) = build_runtime(
        config,
        &prepared.session,
        "Compact the saved context",
        Launch {
            permissions: config.permissions_mode,
            store,
            cancellation: cancel,
            confirmation: None,
            input: None,
            approvals: Preauthorizations::default(),
            personal_context: personal_context(cli, config)?,
            anthropic_prompt_cache: cli.anthropic_prompt_cache,
            events,
            run_id: Some(run_id),
        },
    )
    .await?;
    let result = runtime.compact(&mut prepared.session).await?;
    log_run_result(config, &result);
    match config.output {
        Output::Jsonl => Ok(status_code(result.status)),
        Output::Json => report_json(&result, config),
        Output::Text => Ok(report_result(&result, &prepared.session, false, config)),
    }
}
async fn build_runtime(
    config: &Config,
    session: &Session,
    summary: &str,
    launch: Launch,
) -> Result<(AgentRuntime, bool), Box<dyn std::error::Error>> {
    install_diagnostics(config)?;
    let events = launch.events;
    let app_launch = soyo_app::Launch {
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
    };
    // The build opens stores and may hash the worker installation tree; run it
    // on a blocking thread so Ctrl-C handling on this current_thread runtime
    // is not delayed behind the synchronous work.
    soyo_app::spawn_build_runtime(
        config.clone(),
        session.clone(),
        summary.to_owned(),
        app_launch,
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
    .await
    .map_err(|error| -> Box<dyn std::error::Error> { error })
}
fn log_warning(config: &Config, level: &str, event: &str, data: serde_json::Value) {
    let Ok(secrets) = config_secrets(config) else {
        eprintln!("soyo: structured log unavailable");
        return;
    };
    let log = soyo_core::logging::JsonLog::new(&config.state_dir, secrets)
        .with_level(LOG_LEVEL.get().copied().unwrap_or_default());
    if log.record(level, event, data).is_err() {
        eprintln!("soyo: structured log unavailable");
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
    result: &soyo_core::contract::RunResult,
    session: &Session,
    streamed: bool,
    config: &Config,
) -> u8 {
    let clarification = session.history.iter().rev().find_map(|item| match item {
        HistoryItem::ToolResult { result, .. }
            if result
                .error
                .as_ref()
                .is_some_and(|e| e.code == "needs_input") =>
        {
            result.data.as_ref().map(|data| data.to_string())
        }
        _ => None,
    });
    report_result_inner(result, clarification, streamed, config)
}
/// Shared tail of run reporting. `clarification` is the rendered needs_input
/// question — from session history in-process, from the helper over socket.
fn report_result_inner(
    result: &soyo_core::contract::RunResult,
    clarification: Option<String>,
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
        eprintln!("soyo: cannot write text output");
        return 1;
    }
    if result.stop_reason.as_deref() == Some("completed_with_background_cleanup") {
        eprintln!("soyo: unfinished background jobs were cancelled when the run ended");
    }
    if result.status == RunStatus::NeedsInput
        && let Some(question) = &clarification
    {
        eprintln!("soyo: clarification: {}", safe_output(question, config));
    }
    if let Some(e) = &result.error {
        eprintln!(
            "soyo: {}: {}",
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
        let secrets = config_secrets(config)?;
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
            "soyo: recovered {} interrupted run(s); {} tool outcome(s) unknown; no tools were replayed",
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
        let secrets = config_secrets(config)?;
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
    )
    .await?;
    if let Some(events) = &events {
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
                let result = failed_run_result(session.id, run_id, error.0.clone());
                if let Err(output_error) = events.emit_failure(result).await {
                    report_runtime_error(&output_error, config);
                }
                return Ok(1);
            }
            if config.output == Output::Json {
                let result = failed_run_result(session.id, run_id, error.0);
                return report_json(&result, config);
            }
            Ok(1)
        }
    }
}
/// Runtime payload for a single `soyo run`, shared by the in-process and
/// socket-backed paths.
struct RunPayload {
    text: String,
    personal_context: PersonalContext,
    anthropic_prompt_cache: bool,
    resume_options: session::ResumeOptions,
}
/// `soyo run` through the shared helper daemon. Falls back to the in-process
/// path when the helper cannot start or when config-source overrides
/// (`--config`/`--data-dir`) cannot cross the socket boundary.
async fn run_via_helper(
    config: &Config,
    cli: &Cli,
    args: &RunArgs,
    payload: RunPayload,
) -> Result<u8, Box<dyn std::error::Error>> {
    if cli.config.is_some() || cli.data_dir.is_some() {
        return run(
            config,
            args,
            payload.text,
            payload.personal_context,
            payload.anthropic_prompt_cache,
            payload.resume_options,
        )
        .await;
    }
    match HelperClient::connect_or_spawn().await {
        Ok(client) => match run_over_socket(&client, config, cli, args, payload).await {
            Ok(code) => Ok(code),
            Err(err) => {
                client.detach().await;
                Err(err)
            }
        },
        Err(err) => {
            eprintln!(
                "soyo: helper unavailable ({}); using in-process runtime",
                safe_output(&err.message, config)
            );
            run(
                config,
                args,
                payload.text,
                payload.personal_context,
                payload.anthropic_prompt_cache,
                payload.resume_options,
            )
            .await
        }
    }
}
fn approval_request_from_dto(dto: &ApprovalRequestDto) -> Option<ApprovalRequest> {
    Some(ApprovalRequest {
        approval_id: dto.approval_id.clone(),
        call_id: dto.call_id.clone(),
        run_id: serde_json::from_value(serde_json::Value::String(dto.run_id.clone())).ok()?,
        tool: dto.tool.clone(),
        operation_summary: dto.operation_summary.clone(),
        arguments: dto.arguments.clone(),
        cwd: PathBuf::from(&dto.cwd),
        work_mode: dto.work_mode.into(),
    })
}
fn helper_boundary_error(err: &DesktopError) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: err.code.clone(),
            message: err.message.clone(),
        },
        None,
        None,
    )
}
async fn run_over_socket(
    client: &Arc<HelperClient>,
    config: &Config,
    cli: &Cli,
    args: &RunArgs,
    payload: RunPayload,
) -> Result<u8, Box<dyn std::error::Error>> {
    let RunPayload {
        text,
        personal_context,
        anthropic_prompt_cache,
        resume_options,
    } = payload;
    let terminal = if args.interactive {
        let secrets = config_secrets(config)?;
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
    let cancel = Arc::new(Cancel::new());
    let _signals = signals::CancellationListener::start(cancel.clone())?;
    // Forward process cancellation to the run inside the helper.
    {
        let watch_cancel = cancel.clone();
        let watch_client = client.clone();
        tokio::spawn(async move {
            watch_cancel.cancelled().await;
            let _ = watch_client.call("cancel_run", serde_json::json!({})).await;
        });
    }

    let params = RunPromptParams {
        prompt: text,
        session_id: args.session.clone(),
        accept_config_change: args.accept_config_change,
        cwd: cli.cwd.as_ref().map(|p| p.to_string_lossy().into_owned()),
        profile: cli.profile.clone(),
        mode: cli.mode.map(|m| WorkMode::from(m).into()),
        permissions: cli.permissions.map(|p| PermissionMode::from(p).into()),
        approval_file: args
            .approval_file
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned()),
        interactive: terminal.is_some(),
        anthropic_prompt_cache,
        personal_context: Some(PersonalContextDto {
            preferences: personal_context.preferences.clone(),
            memory: personal_context.memory.clone(),
        }),
        attachments: Vec::new(),
    };

    // `params` moves into the streaming call; keep the fields the in-process
    // fallback needs to reconstruct the original request.
    let fallback_text = params.prompt.clone();
    let fallback_personal = personal_context;
    let fallback_cache = params.anthropic_prompt_cache;

    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let call_client = client.clone();
    let call = tokio::spawn(async move {
        call_client
            .call_streaming::<_, SubmitPromptResultDto>("run_prompt", &params, events_tx)
            .await
    });

    let mut saw_event = false;
    let mut streamed_text = false;
    let mut finished: Option<RunResult> = None;
    let mut completed_error: Option<DesktopError> = None;
    let mut needs_input_question: Option<String> = None;
    while let Some(envelope) = events_rx.recv().await {
        saw_event = true;
        match envelope {
            DesktopStreamEnvelope::RawSchemaEvent { event } => {
                if let Ok(schema_event) = serde_json::from_value::<Event>(event.clone()) {
                    match schema_event.payload {
                        EventPayload::ModelTextDelta { text, .. }
                            if config.output == Output::Text =>
                        {
                            let mut stdout = io::stdout().lock();
                            let _ = stdout
                                .write_all(safe_terminal_text(&text).as_bytes())
                                .and_then(|()| stdout.flush());
                            streamed_text = true;
                        }
                        EventPayload::RunFinished(result) => finished = Some(result),
                        _ => {}
                    }
                }
                if config.output == Output::Jsonl {
                    let mut stdout = io::stdout().lock();
                    serde_json::to_writer(&mut stdout, &event)
                        .map_err(|_| cli_error("output_error", "cannot write event"))?;
                    stdout
                        .write_all(b"\n")
                        .and_then(|()| stdout.flush())
                        .map_err(|_| cli_error("output_error", "cannot write event"))?;
                }
            }
            DesktopStreamEnvelope::ApprovalNeeded(dto) => {
                if let Some(term) = &terminal {
                    let approved = match approval_request_from_dto(&dto) {
                        Some(req) => term
                            .confirm(&req, cancel.as_ref())
                            .await
                            .ok()
                            .flatten()
                            .unwrap_or(false),
                        None => false,
                    };
                    let _ = client
                        .call(
                            "reply_approval",
                            serde_json::json!({
                                "approval_id": dto.approval_id,
                                "call_id": dto.call_id,
                                "approved": approved,
                            }),
                        )
                        .await;
                }
            }
            DesktopStreamEnvelope::InputNeeded(dto) => {
                if let Some(term) = &terminal {
                    let answer = term
                        .ask(
                            &InputRequest {
                                question: dto.question,
                                options: dto.options,
                            },
                            cancel.as_ref(),
                        )
                        .await
                        .ok()
                        .flatten();
                    let _ = client
                        .call(
                            "reply_input",
                            serde_json::json!({
                                "input_id": dto.input_id,
                                "answer": answer,
                            }),
                        )
                        .await;
                }
            }
            DesktopStreamEnvelope::RunCompleted {
                error,
                needs_input_question: question,
                ..
            } => {
                completed_error = error;
                needs_input_question = question;
            }
            _ => {}
        }
    }

    // The terminal response resolves once the run driver finishes forwarding.
    let started = match call.await {
        Ok(Ok(dto)) => dto,
        Ok(Err(err)) => {
            if !saw_event {
                // The run never started inside the helper; the in-process path
                // preserves the standalone `soyo run` semantics.
                eprintln!(
                    "soyo: helper run rejected ({}: {}); using in-process runtime",
                    err.code,
                    safe_output(&err.message, config)
                );
                return run(
                    config,
                    args,
                    fallback_text,
                    fallback_personal,
                    fallback_cache,
                    resume_options,
                )
                .await;
            }
            return Err(Box::new(helper_boundary_error(&err)));
        }
        Err(_) => return Err("helper run task failed".into()),
    };

    if started.recovered_runs.unwrap_or(0) > 0 {
        eprintln!(
            "soyo: recovered {} interrupted run(s); {} tool outcome(s) unknown; no tools were replayed",
            started.recovered_runs.unwrap_or(0),
            started.recovered_tools.unwrap_or(0)
        );
    }

    match (finished, completed_error) {
        (Some(result), error) => {
            if let Some(err) = error {
                // Boundary/runtime failure after events started; mirror the
                // in-process Err arm: emit_failure already printed (JSONL),
                // JSON re-reports the result, text reports the error.
                log_warning(
                    config,
                    "error",
                    "run.failed",
                    serde_json::json!({"session_id":started.session_id,"run_id":started.run_id,"error":{"code":err.code,"message":err.message}}),
                );
                report_runtime_error(&helper_boundary_error(&err), config);
                return match config.output {
                    Output::Json => report_json(&result, config),
                    _ => Ok(1),
                };
            }
            log_run_result(config, &result);
            match config.output {
                Output::Jsonl => Ok(status_code(result.status)),
                Output::Json => report_json(&result, config),
                Output::Text => Ok(report_result_inner(
                    &result,
                    needs_input_question,
                    streamed_text,
                    config,
                )),
            }
        }
        (None, error) => {
            // No run.finished reached the wire (boundary failure before or
            // during emit_failure): same reporting as the in-process Err arm.
            let err = error.unwrap_or_else(|| {
                DesktopError::new("run_terminated", "Run ended without a result")
            });
            log_warning(
                config,
                "error",
                "run.failed",
                serde_json::json!({"session_id":started.session_id,"run_id":started.run_id,"error":{"code":err.code,"message":err.message}}),
            );
            report_runtime_error(&helper_boundary_error(&err), config);
            if config.output == Output::Json {
                let session_id = serde_json::from_value::<SessionId>(serde_json::Value::String(
                    started.session_id,
                ))
                .unwrap_or_default();
                let run_id =
                    serde_json::from_value::<RunId>(serde_json::Value::String(started.run_id))
                        .unwrap_or_default();
                let result = failed_run_result(
                    session_id,
                    run_id,
                    ErrorInfo {
                        code: err.code,
                        message: err.message,
                    },
                );
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
    let store = {
        let owned = config.clone();
        soyo_app::offload(move || session::open(&owned)).await?
    };
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
    soyo_core::redaction::redact(&mut data, &secrets)?;
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
async fn launch_tui(cli: &Cli, overrides: Overrides, environment: Environment) -> u8 {
    let mut config = match load_config(overrides, environment) {
        Ok(config) => config,
        Err(error) => return fail(error),
    };
    if let Err(error) = soyo_app::apply_module_overrides(&mut config) {
        return fail(safe_output(&error.to_string(), &config));
    }
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
    let Ok(secrets) = config_secrets(&config) else {
        return fail("sensitive settings unavailable");
    };
    let options = soyo_tui::Options {
        personal_context: context,
        anthropic_prompt_cache: cli.anthropic_prompt_cache,
        cwd_explicit: cli.cwd.is_some(),
        profile_explicit: cli.profile.is_some(),
        mode_override: cli.mode.map(Into::into),
    };
    match soyo_tui::run(config, options).await {
        Ok(code) => code,
        Err(error) => tui_error(&redacted_text(&error.to_string(), &secrets)),
    }
}

async fn run_chat_session(cli: &Cli, config: Config, resume: Option<chat::RemoteResume>) -> u8 {
    let personal = match personal_context(cli, &config) {
        Ok(personal) => personal,
        Err(e) => return fail(safe_output(&e.to_string(), &config)),
    };
    let Ok(secrets) = config_secrets(&config) else {
        return fail("sensitive details unavailable");
    };
    if cli.config.is_some() || cli.data_dir.is_some() {
        // Explicit config/data-dir overrides bypass the shared helper — the
        // same contract `soyo run` uses, since the daemon cannot honor
        // per-invocation storage paths.
        let prepared = match resume {
            Some(resume) => match session::resume(
                &config,
                &resume.session_id,
                session::ResumeOptions {
                    accept_config_change: resume.accept_config_change,
                    cwd_explicit: resume.cwd_explicit,
                    profile_explicit: resume.profile_explicit,
                    mode_override: resume.mode_override,
                },
            )
            .await
            {
                Ok(prepared) => Some(prepared),
                Err(e) => return fail(safe_output(&e.to_string(), &config)),
            },
            None => None,
        };
        return match chat::chat(config, personal, cli.anthropic_prompt_cache, prepared).await {
            Ok(code) => code,
            Err(e) => fail(redacted_text(&e.to_string(), &secrets)),
        };
    }
    match chat::chat_via_helper(config, personal, cli.anthropic_prompt_cache, resume).await {
        Ok(code) => code,
        Err(e) => fail(redacted_text(&e.to_string(), &secrets)),
    }
}

/// Start the shared helper if it is not running yet. Interactive entry points
/// (TUI, chat, resume) call this before doing their own runtime work; failure
/// is a warning, not fatal — the CLI still runs its own embedded runtime.
async fn ensure_helper_started() {
    if let Err(err) = helper_commands::ensure_helper().await {
        eprintln!("soyo: cannot start helper daemon ({err}); continuing without shared backend");
    }
}

async fn dispatch_command(cli: &Cli, overrides: Overrides, env: Environment) -> u8 {
    match &cli.command {
        // The preview is dispatched before runtime configuration and environment capture.
        #[cfg(debug_assertions)]
        Some(Command::TuiPreview { .. }) => fail("TUI preview dispatch is unavailable"),
        None => {
            ensure_helper_started().await;
            launch_tui(cli, overrides, env).await
        }
        Some(Command::Logs { action }) => match load_config(overrides, env) {
            Err(error) => query_error(error, cli.output.map(Into::into)),
            Ok(config) => match logs::run(&config, action).await {
                Ok(code) => code,
                Err(error) => query_error(
                    boxed_error(&*error, "invalid_query", &config),
                    Some(config.output),
                ),
            },
        },
        Some(Command::Mcp { action }) => match load_config(overrides, env) {
            Err(error) => query_error(error, cli.output.map(Into::into)),
            Ok(config) => mcp::run(&config, action).await,
        },
        Some(Command::Goal { action }) => match load_config(overrides, env) {
            Err(error) => query_error(error, cli.output.map(Into::into)),
            Ok(config) => match goal_commands::run(action, cli, &config).await {
                Ok(code) => code,
                Err(error) => query_error(sanitized_error(&error, &config), Some(config.output)),
            },
        },
        Some(Command::Memory { action }) => match load_config(overrides, env) {
            Err(error) => query_error(error, cli.output.map(Into::into)),
            Ok(config) => administer_memory(action, &config).await,
        },
        Some(Command::Plugin { action }) => match load_config(overrides, env) {
            Err(error) => query_error(error, cli.output.map(Into::into)),
            Ok(config) => match plugin_commands::run(action, &config).await {
                Ok(mut value) => {
                    if config.output == Output::Jsonl {
                        value["type"] = serde_json::json!("plugin.result");
                    }
                    match writeln!(io::stdout().lock(), "{value}") {
                        Ok(()) => 0,
                        Err(_) => query_error(
                            cli_error("runtime_error", "cannot write plugin result"),
                            Some(config.output),
                        ),
                    }
                }
                Err(error) => query_error(sanitized_error(&error, &config), Some(config.output)),
            },
        },
        Some(Command::Module(args)) => match load_config(overrides, env) {
            Err(error) => query_error(error, cli.output.map(Into::into)),
            Ok(config) => {
                let module = {
                    let args = args.clone();
                    let owned = config.clone();
                    soyo_app::offload(move || {
                        module_commands::run(&args, &owned).map_err(|error| error.into())
                    })
                    .await
                };
                match module {
                    Ok(output) => match module_commands::write_line(&output) {
                        Ok(()) => 0,
                        Err(_) => query_error(
                            cli_error("runtime_error", "cannot write module result"),
                            Some(config.output),
                        ),
                    },
                    Err(error) => query_error(
                        boxed_error(&*error, "invalid_query", &config),
                        Some(config.output),
                    ),
                }
            }
        },
        Some(Command::Helper) => helper_commands::run_helper().await,
        Some(Command::Kill) => helper_commands::kill_helper().await,
        Some(Command::Config {
            action: ConfigAction::Init,
        }) => match init(&overrides, &env) {
            Ok(()) => {
                println!("Configuration template created.");
                0
            }
            Err(e) => fail(e),
        },
        Some(Command::Config {
            action: ConfigAction::Check,
        }) => match load_config(overrides, env) {
            Ok(mut config) => match soyo_app::apply_module_overrides(&mut config) {
                Ok(_) => {
                    println!("Configuration is valid.");
                    0
                }
                Err(error) => fail(safe_output(&error.to_string(), &config)),
            },
            Err(e) => fail(e),
        },
        Some(Command::Compact {
            session_id,
            accept_config_change,
        }) => match load_config(overrides, env) {
            Err(error) => output_error(error, cli.output.map(Into::into)),
            Ok(config) => {
                match compact_session(cli, &config, session_id, *accept_config_change).await {
                    Ok(code) => code,
                    Err(error) => output_error(
                        boxed_error(&*error, "invalid_input", &config),
                        Some(config.output),
                    ),
                }
            }
        },
        Some(Command::Run(args)) => dispatch_run_command(cli, args, overrides, env).await,
        Some(Command::Chat) => match load_config(overrides, env) {
            Err(e) => fail(e),
            Ok(config) => {
                ensure_helper_started().await;
                run_chat_session(cli, config, None).await
            }
        },
        Some(Command::Session { action }) => match load_config(overrides, env) {
            Err(e) => query_error(e, cli.output.map(Into::into)),
            Ok(config) => match query_sessions(&config, action).await {
                Ok(code) => code,
                Err(e) => query_error(
                    boxed_error(&*e, "invalid_query", &config),
                    Some(config.output),
                ),
            },
        },
        Some(Command::Resume {
            session_id,
            accept_config_change,
        }) => match load_config(overrides, env) {
            Err(e) => fail(e),
            Ok(config) => {
                ensure_helper_started().await;
                run_chat_session(
                    cli,
                    config,
                    Some(chat::RemoteResume {
                        session_id: session_id.clone(),
                        accept_config_change: *accept_config_change,
                        cwd_explicit: cli.cwd.is_some(),
                        profile_explicit: cli.profile.is_some(),
                        mode_override: cli.mode.map(Into::into),
                    }),
                )
                .await
            }
        },
    }
}

async fn dispatch_run_command(
    cli: &Cli,
    args: &RunArgs,
    overrides: Overrides,
    env: Environment,
) -> u8 {
    let config = match load_config(overrides, env) {
        Ok(config) => config,
        Err(e) => return output_error(e, cli.output.map(Into::into)),
    };
    let text = match prompt(args) {
        Ok(text) => text,
        Err(e) => {
            return output_error(
                boxed_error(&*e, "invalid_input", &config),
                Some(config.output),
            );
        }
    };
    let personal = match personal_context(cli, &config) {
        Ok(personal) => personal,
        Err(e) => {
            return output_error(
                boxed_error(&*e, "invalid_input", &config),
                Some(config.output),
            );
        }
    };
    let output = config.output;
    match run_via_helper(
        &config,
        cli,
        args,
        RunPayload {
            text,
            personal_context: personal,
            anthropic_prompt_cache: cli.anthropic_prompt_cache,
            resume_options: session::ResumeOptions {
                accept_config_change: args.accept_config_change,
                cwd_explicit: cli.cwd.is_some(),
                profile_explicit: cli.profile.is_some(),
                mode_override: cli.mode.map(Into::into),
            },
        },
    )
    .await
    {
        Ok(code) => code,
        Err(e) => {
            if args.session.is_none()
                && session::check_roots(&config, &config.cwd, config.mode).is_ok()
            {
                log_warning(
                    &config,
                    "error",
                    "run.setup_failed",
                    serde_json::json!({"message": safe_output(&e.to_string(), &config)}),
                );
            }
            output_error(boxed_error(&*e, "invalid_input", &config), Some(output))
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    if let Some(code) =
        soyo_core::execution::internal_command(std::env::args_os().skip(1).collect()).await
    {
        return std::process::ExitCode::from(u8::try_from(code).unwrap_or(1));
    }
    if soyo_core::execution::pin_launcher_image().is_err() {
        return std::process::ExitCode::from(fail("cannot prepare the Soyo execution image"));
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
        return std::process::ExitCode::from(match soyo_tui::preview::run(scene, *source).await {
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
                "TUI requires terminal stdin and stdout. Use soyo run --stdin or soyo chat",
            ));
        }
    }
    let raw_level = cli
        .log_level
        .clone()
        .or_else(|| {
            std::env::var("SOYO_LOG_LEVEL")
                .or_else(|_| std::env::var("SAYA_LOG_LEVEL"))
                .ok()
        })
        .unwrap_or_else(|| "warn".into());
    let level = match raw_level.parse::<soyo_core::logging::LogLevel>() {
        Ok(level) => level,
        Err(_) => {
            return std::process::ExitCode::from(output_error(
                cli_error("invalid_args", "invalid log level"),
                cli.output.map(Into::into),
            ));
        }
    };
    let _ = LOG_LEVEL.set(level);
    let code = dispatch_command(&cli, overrides(&cli), Environment::capture()).await;
    if let Ok(mut diagnostics) = DIAGNOSTICS.lock()
        && let Some(diagnostics) = diagnostics.take()
        && diagnostics
            .flush(std::time::Duration::from_secs(2))
            .is_err()
    {
        if cli.command.is_none() {
            tui_error("diagnostic log flush failed");
        } else {
            eprintln!("soyo: diagnostic log flush failed");
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
            let cli = Cli::try_parse_from(["soyo", "tui-preview", "markdown", "--source"]).unwrap();
            assert!(
                matches!(cli.command, Some(Command::TuiPreview { scene, source: true }) if scene == "markdown")
            );
            assert!(Cli::try_parse_from(["soyo", "tui-preview", "unknown-scene"]).is_err());
        }
        #[cfg(not(debug_assertions))]
        assert!(Cli::try_parse_from(["soyo", "tui-preview"]).is_err());
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
    #[test]
    fn memory_failure_keeps_observed_receipt_structured_and_redactable() {
        let action = memory_commands::MemoryAction::Recover;
        let failure = BoundaryError(
            ErrorInfo {
                code: "outcome_unknown".into(),
                message: "Publication needs recovery".into(),
            },
            None,
            Some(Box::new(soyo_core::contract::ToolResult {
                ok: false,
                data: Some(
                    serde_json::json!({"status":"unknown","receipt":{"operation_id":"op_explicit","detail":"test-private-value"},"current_revision":"a".repeat(64)}),
                ),
                error: None,
                truncated: false,
                media_refs: Vec::new(),
            })),
        );
        let mut value = memory_failure_value(&action, failure);
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["code"], "outcome_unknown");
        assert_eq!(
            value["actual_result"]["data"]["receipt"]["operation_id"],
            "op_explicit"
        );
        soyo_core::redaction::redact(&mut value, &["test-private-value".into()]).unwrap();
        assert!(!value.to_string().contains("test-private-value"));
        assert!(value["actual_result"].is_object());
    }
}
