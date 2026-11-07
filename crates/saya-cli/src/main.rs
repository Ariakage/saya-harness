mod chat;
mod session;

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use saya_core::{
    approvals::Preauthorizations,
    boundary::{Cancellation, Model, ModelRequest, ModelStream, ModelUpdate, Operation, Store},
    config::{self, Auth, Config, Environment, Output, Overrides, Profile, Protocol},
    contract::{
        ApprovalRequest, BoundaryError, HistoryItem, ModelProtocol, ModelPurpose, PermissionMode,
        Run, RunId, RunStatus, Session, SessionId, WorkMode,
    },
    input::{InputHandler, InputRequest},
    permissions::Policy,
    project_context::ProjectContext,
    provider::{
        ModelSettings,
        anthropic::{AnthropicModel, AnthropicVersion},
        chat::{ChatCompatibility, ChatModel},
        responses::{ResponsesCompatibility, ResponsesModel},
        router::ProfileRouter,
        transport::{Authentication, HttpConfig, HttpTransport},
    },
    review::ReviewConfig,
    runtime::{AgentRuntime, ApprovalHandler, PersonalContext, RuntimeLimits, RuntimeSetup},
    tools::{
        exec::ExecConfig,
        read::{ReadLimits, ReadTools},
    },
};
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    future::Future,
    io::{self, Read, Write},
    path::PathBuf,
    pin::Pin,
    sync::Arc,
    time::Duration,
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
struct Cancel(tokio::sync::watch::Sender<bool>);
impl Cancellation for Cancel {
    fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let mut rx = self.0.subscribe();
            while !*rx.borrow_and_update() {
                if rx.changed().await.is_err() {
                    break;
                }
            }
        })
    }
}
struct Terminal(tokio::sync::Mutex<std::fs::File>);
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
fn redact_value(value: &mut serde_json::Value, secrets: &[String], depth: usize) {
    match value {
        serde_json::Value::String(text) => {
            for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
                *text = text.replace(secret, "[REDACTED]");
            }
            if depth < 4
                && let Ok(mut nested) = serde_json::from_str::<serde_json::Value>(text)
                && !matches!(&nested, serde_json::Value::String(inner) if inner == text)
            {
                redact_value(&mut nested, secrets, depth + 1);
                if let Ok(encoded) = serde_json::to_string(&nested) {
                    *text = encoded;
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                redact_value(item, secrets, depth);
            }
        }
        serde_json::Value::Object(items) => {
            let original = std::mem::take(items);
            for (mut key, mut item) in original {
                for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
                    key = key.replace(secret, "[REDACTED]");
                }
                redact_value(&mut item, secrets, depth);
                items.insert(key, item);
            }
        }
        _ => {}
    }
}
fn contains_encoded_secret(text: &str, secrets: &[String]) -> bool {
    secrets
        .iter()
        .filter(|secret| !secret.is_empty())
        .any(|secret| {
            let mut candidate = secret.clone();
            for _ in 0..6 {
                if text.contains(&candidate) {
                    return true;
                }
                let Ok(encoded) = serde_json::to_string(&candidate) else {
                    return true;
                };
                let Some(fragment) = encoded.get(1..encoded.len().saturating_sub(1)) else {
                    return true;
                };
                if fragment == candidate {
                    break;
                }
                candidate = fragment.to_owned();
            }
            false
        })
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
    fn open() -> io::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open("/dev/tty")?;
        Ok(Self(tokio::sync::Mutex::new(file)))
    }
    async fn read_line(
        &self,
        prompt: &str,
        cancel: &dyn Cancellation,
    ) -> Result<Option<String>, BoundaryError> {
        let mut tty = self.0.lock().await;
        write!(tty, "{prompt}")
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
fn model(
    config: &Config,
    name: &str,
    p: &Profile,
    anthropic_prompt_cache: bool,
) -> Result<Arc<dyn Model>, BoundaryError> {
    let auth = match p.auth {
        Auth::None => Authentication::None,
        Auth::Bearer => Authentication::Bearer(
            config
                .secret(p.api_key_env.as_deref().unwrap_or_default())?
                .to_owned(),
        ),
        Auth::ApiKey => Authentication::ApiKey(
            config
                .secret(p.api_key_env.as_deref().unwrap_or_default())?
                .to_owned(),
        ),
    };
    let t = Arc::new(HttpTransport::new(HttpConfig {
        base_url: p.base_url.clone(),
        allow_remote_http: p.allow_remote_http,
        authentication: auth,
        headers_env: p.headers_env.clone(),
        connect_timeout: Duration::from_secs(config.limits.model_connect_timeout_seconds),
        idle_timeout: Duration::from_secs(config.limits.model_idle_timeout_seconds),
        total_timeout: Duration::from_secs(config.limits.model_timeout_seconds),
    })?);
    let settings = ModelSettings {
        profile: name.into(),
        model: p.model.clone(),
        context_window_tokens: p.context_window_tokens,
        max_output_tokens: p.max_output_tokens,
        strict_tools: p.strict_tools,
        stream: p.stream,
    };
    Ok(match p.protocol {
        Protocol::ChatCompletions => Arc::new(ChatModel::new(
            t,
            settings,
            ChatCompatibility {
                legacy_max_tokens: p.legacy_max_tokens,
                omit_stream_usage: p.omit_stream_usage,
            },
        )),
        Protocol::Responses => Arc::new(ResponsesModel::new(
            t,
            settings,
            ResponsesCompatibility {
                omit_reasoning_include: p.omit_reasoning_include,
            },
        )),
        Protocol::AnthropicMessages => Arc::new(
            AnthropicModel::new(
                t,
                settings,
                if p.api_version == "2023-06-01" {
                    AnthropicVersion::V20230601
                } else {
                    AnthropicVersion::V20261021
                },
            )
            .with_prompt_cache(anthropic_prompt_cache),
        ),
    })
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
fn new_session(config: &Config) -> Result<Session, Box<dyn std::error::Error>> {
    let profile = config.profile.as_ref().ok_or("no profile selected")?;
    let selected = config.profiles.get(profile).ok_or("profile not found")?;
    let protocol = match selected.protocol {
        Protocol::ChatCompletions => ModelProtocol::ChatCompletions,
        Protocol::Responses => ModelProtocol::Responses,
        Protocol::AnthropicMessages => ModelProtocol::AnthropicMessages,
    };
    Ok(Session {
        id: SessionId::new(),
        workspace: std::fs::canonicalize(&config.cwd)?,
        work_mode: config.mode,
        profile: profile.clone(),
        protocol,
        model: selected.model.clone(),
        history: vec![],
    })
}
fn build_runtime(
    config: &Config,
    session: &Session,
    summary: &str,
    launch: Launch,
) -> Result<(AgentRuntime, bool), Box<dyn std::error::Error>> {
    let profile = &session.profile;
    let selected = config.profiles.get(profile).ok_or("profile not found")?;
    let run = Run {
        id: RunId::new(),
        session_id: session.id,
        work_mode: session.work_mode,
        permissions: launch.permissions,
    };
    let models = config
        .profiles
        .iter()
        .map(|(name, p)| {
            Ok((
                name.clone(),
                model(
                    config,
                    name,
                    p,
                    launch.anthropic_prompt_cache && name == profile,
                )?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>, BoundaryError>>()?;
    let router = Arc::new(ProfileRouter::new(profile.clone(), models)?);
    let streamed = selected.stream;
    let model: Arc<dyn Model> = if streamed {
        Arc::new(PrintModel(router))
    } else {
        router
    };
    let secrets = config.secret_env_names();
    let known_secrets = secrets
        .iter()
        .map(|name| config.secret(name).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    let policy = Arc::new(Policy::new(&session.workspace, config.permissions.clone())?);
    let lim = &config.limits;
    let files = Arc::new(ReadTools::new(
        &session.workspace,
        &config.permissions.additional_roots,
        launch.permissions == PermissionMode::Bypass,
        ReadLimits {
            read_return_bytes: lim.read_return_max_bytes,
            file_bytes: lim.read_file_max_bytes,
            search_results: lim.search_max_results,
            search_bytes: lim.search_return_max_bytes,
            list_entries: lim.list_max_entries,
            list_bytes: lim.list_return_max_bytes,
        },
    )?);
    let timeout = |seconds| Duration::from_secs(seconds);
    let exec = ExecConfig {
        default_timeout: timeout(lim.command_timeout_seconds),
        max_timeout: timeout(lim.max_command_timeout_seconds),
        stdout_max: lim.command_stdout_max_bytes,
        stderr_max: lim.command_stderr_max_bytes,
        inherit_extra: config.commands.inherit_env.clone(),
        secret_env_names: secrets,
    };
    let limits = RuntimeLimits {
        model_requests: lim.max_model_requests,
        tool_calls: lim.max_tool_calls,
        active_time: timeout(lim.active_run_timeout_seconds),
        model_connect: timeout(lim.model_connect_timeout_seconds),
        model_idle: timeout(lim.model_idle_timeout_seconds),
        model_total: timeout(lim.model_timeout_seconds),
        tool_timeout: timeout(lim.command_timeout_seconds),
        background_running: lim.max_background_running,
        background_jobs: lim.max_background_jobs,
        ..Default::default()
    };
    let mut review = ReviewConfig::new(profile.clone());
    review.review_profile = config.review.profile.clone();
    review.max_output_tokens = config.review.max_output_tokens;
    review.connect_timeout = limits.model_connect;
    review.idle_timeout = limits.model_idle;
    review.total_timeout = limits.model_total;
    review.known_secrets = known_secrets.clone();
    let mut runtime = AgentRuntime::new(RuntimeSetup {
        run,
        model,
        store: launch.store,
        policy,
        files,
        cancellation: launch.cancellation,
        review,
        exec,
        preauthorizations: launch.approvals,
        known_secrets,
        summary: summary.to_owned(),
        confirmation: launch.confirmation,
        limits,
    })?;
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    runtime =
        runtime.with_project_context(ProjectContext::new(&session.workspace, home.as_deref())?)?;
    if let Some(input) = launch.input {
        runtime = runtime.with_input_handler(input);
    }
    runtime = runtime.with_personal_context(launch.personal_context);
    Ok((runtime, streamed))
}
fn report_result(result: &saya_core::contract::RunResult, session: &Session, streamed: bool) -> u8 {
    if streamed {
        println!();
    } else if let Some(text) = &result.text {
        println!("{text}");
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
            safe_terminal_text(&question.to_string())
        );
    }
    if let Some(e) = &result.error {
        eprintln!("saya: {}: {}", e.code, e.message);
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
async fn run(
    config: Config,
    args: &RunArgs,
    text: String,
    personal_context: PersonalContext,
    anthropic_prompt_cache: bool,
    cwd_explicit: bool,
    profile_explicit: bool,
) -> Result<u8, Box<dyn std::error::Error>> {
    if config.output != Output::Text {
        return Err("JSON and JSONL output are not available until P6".into());
    }
    let terminal = if args.interactive {
        match Terminal::open() {
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
        session::resume(
            &config,
            id,
            args.accept_config_change,
            cwd_explicit,
            profile_explicit,
        )
        .await?
    } else {
        session::create(&config, new_session(&config)?).await?
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
    let cancel = Arc::new(Cancel(tokio::sync::watch::channel(false).0));
    let signal = cancel.clone();
    let mut interrupts = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    tokio::spawn(async move {
        if interrupts.recv().await.is_some() {
            signal.0.send_replace(true);
            if interrupts.recv().await.is_some() {
                std::process::exit(130);
            }
        }
    });
    let confirmation = terminal
        .as_ref()
        .map(|t| t.clone() as Arc<dyn ApprovalHandler>);
    let input = terminal.map(|t| t as Arc<dyn InputHandler>);
    let (mut runtime, streamed) = build_runtime(
        &config,
        &session,
        &text,
        Launch {
            permissions: config.permissions_mode,
            store: prepared.store.clone(),
            cancellation: cancel,
            confirmation,
            input,
            approvals,
            personal_context,
            anthropic_prompt_cache,
        },
    )?;
    match runtime.run(&mut session).await {
        Ok(result) => Ok(report_result(&result, &session, streamed)),
        Err(error) => {
            report_runtime_error(&error, &config);
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
    let data = match action {
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
            session
        }
    };
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
        Some(Command::Run(args)) => match prompt(args) {
            Err(e) => fail(e),
            Ok(text) => match std::env::current_dir()
                .map_err(|_| "cannot determine current directory".to_owned())
                .and_then(|cwd| config::load(o, env, &cwd).map_err(|e| e.to_string()))
            {
                Err(e) => fail(e),
                Ok(c) => match personal_context(&cli, &c) {
                    Err(e) => fail(e),
                    Ok(personal) => {
                        match run(
                            c,
                            args,
                            text,
                            personal,
                            cli.anthropic_prompt_cache,
                            cli.cwd.is_some(),
                            cli.profile.is_some(),
                        )
                        .await
                        {
                            Ok(code) => code,
                            Err(e) => fail(e),
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
                Err(e) => fail(e),
                Ok(personal) => {
                    match chat::chat(config, personal, cli.anthropic_prompt_cache, None).await {
                        Ok(code) => code,
                        Err(e) => fail(e),
                    }
                }
            },
        },
        Some(Command::Session { action }) => match std::env::current_dir()
            .map_err(|_| "cannot determine current directory".to_owned())
            .and_then(|cwd| config::load(o, env, &cwd).map_err(|e| e.to_string()))
        {
            Err(e) => fail(e),
            Ok(config) => match query_sessions(&config, action).await {
                Ok(code) => code,
                Err(e) => fail(e),
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
                Err(e) => fail(e),
                Ok(personal) => match session::resume(
                    &config,
                    session_id,
                    *accept_config_change,
                    cli.cwd.is_some(),
                    cli.profile.is_some(),
                )
                .await
                {
                    Err(e) => fail(e),
                    Ok(prepared) => match chat::chat(
                        config,
                        personal,
                        cli.anthropic_prompt_cache,
                        Some(prepared),
                    )
                    .await
                    {
                        Ok(code) => code,
                        Err(e) => fail(e),
                    },
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
