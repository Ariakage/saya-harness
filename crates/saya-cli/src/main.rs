use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use saya_core::{
    approvals::Preauthorizations,
    boundary::{Cancellation, Model, ModelRequest, ModelStream, ModelUpdate, Operation},
    config::{self, Auth, Config, Environment, Output, Overrides, Profile, Protocol},
    contract::{
        ApprovalRequest, BoundaryError, HistoryItem, ModelProtocol, ModelPurpose, PermissionMode,
        Run, RunId, RunStatus, Session, SessionId, WorkMode,
    },
    input::{InputHandler, InputRequest},
    memory::MemoryStore,
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
    runtime::{AgentRuntime, ApprovalHandler, RuntimeLimits, RuntimeSetup},
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
    Config {
        #[command(subcommand)]
        action: ConfigAction,
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
    let mut text = if let Some(v) = &args.prompt {
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
    if text.ends_with('\n') {
        text.pop();
        if text.ends_with('\r') {
            text.pop();
        }
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
fn model(config: &Config, name: &str, p: &Profile) -> Result<Arc<dyn Model>, BoundaryError> {
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
        Protocol::AnthropicMessages => Arc::new(AnthropicModel::new(
            t,
            settings,
            if p.api_version == "2023-06-01" {
                AnthropicVersion::V20230601
            } else {
                AnthropicVersion::V20261021
            },
        )),
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
async fn run(
    config: Config,
    args: &RunArgs,
    text: String,
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
    let confirmation: Option<Arc<dyn ApprovalHandler>> = terminal
        .as_ref()
        .map(|t| t.clone() as Arc<dyn ApprovalHandler>);
    let profile = config
        .profile
        .as_ref()
        .ok_or("no profile selected for run")?
        .clone();
    let p = config.profiles.get(&profile).ok_or("profile not found")?;
    let protocol = match p.protocol {
        Protocol::ChatCompletions => ModelProtocol::ChatCompletions,
        Protocol::Responses => ModelProtocol::Responses,
        Protocol::AnthropicMessages => ModelProtocol::AnthropicMessages,
    };
    let workspace = std::fs::canonicalize(&config.cwd)?;
    let mut session = Session {
        id: SessionId::new(),
        workspace: workspace.clone(),
        work_mode: config.mode,
        profile: profile.clone(),
        protocol,
        model: p.model.clone(),
        history: vec![HistoryItem::UserText(text.clone())],
    };
    let run = Run {
        id: RunId::new(),
        session_id: session.id,
        work_mode: config.mode,
        permissions: config.permissions_mode,
    };
    let models = config
        .profiles
        .iter()
        .map(|(name, p)| Ok((name.clone(), model(&config, name, p)?)))
        .collect::<Result<BTreeMap<_, _>, BoundaryError>>()?;
    let router = Arc::new(ProfileRouter::new(profile.clone(), models)?);
    let streamed = p.stream;
    let model: Arc<dyn Model> = if streamed {
        Arc::new(PrintModel(router))
    } else {
        router
    };
    let secrets = config.secret_env_names();
    let known_secrets = secrets
        .iter()
        .map(|n| config.secret(n).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    let approvals = if let Some(path) = &args.approval_file {
        Preauthorizations::from_file(path, &workspace)?
    } else {
        Preauthorizations::default()
    };
    let policy = Arc::new(Policy::new(&workspace, config.permissions.clone())?);
    let lim = &config.limits;
    let files = Arc::new(ReadTools::new(
        &workspace,
        &config.permissions.additional_roots,
        config.permissions_mode == PermissionMode::Bypass,
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
    let mut review = ReviewConfig::new(profile);
    review.review_profile = config.review.profile.clone();
    review.max_output_tokens = config.review.max_output_tokens;
    review.connect_timeout = limits.model_connect;
    review.idle_timeout = limits.model_idle;
    review.total_timeout = limits.model_total;
    review.known_secrets = known_secrets.clone();
    let cancel = Arc::new(Cancel(tokio::sync::watch::channel(false).0));
    let signal = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal.0.send_replace(true);
        }
    });
    let mut runtime = AgentRuntime::new_read_only(RuntimeSetup {
        run,
        model,
        store: Arc::new(MemoryStore::default()),
        policy,
        files,
        cancellation: cancel,
        review,
        exec,
        preauthorizations: approvals,
        known_secrets,
        summary: text,
        confirmation,
        limits,
    })?;
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    runtime = runtime.with_project_context(ProjectContext::new(&workspace, home.as_deref())?)?;
    if let Some(terminal) = terminal {
        runtime = runtime.with_input_handler(terminal);
    }
    let result = match runtime.run(&mut session).await {
        Ok(result) => result,
        Err(error) => {
            eprintln!("saya: {error}");
            return Ok(1);
        }
    };
    if streamed {
        println!();
    } else if let Some(text) = &result.text {
        println!("{text}");
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
    Ok(match result.status {
        RunStatus::Completed => 0,
        RunStatus::Failed => 1,
        RunStatus::NeedsConfirmation => 3,
        RunStatus::NeedsInput => 4,
        RunStatus::LimitReached => 5,
        RunStatus::Cancelled => 130,
    })
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
                Ok(c) => match run(c, args, text).await {
                    Ok(code) => code,
                    Err(e) => fail(e),
                },
            },
        },
        Some(Command::Chat) | None => fail("interactive chat is not available yet"),
    };
    std::process::ExitCode::from(code)
}
