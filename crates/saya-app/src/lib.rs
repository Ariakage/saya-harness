//! Shared application assembly and durable session lifecycle for Saya interfaces.
//! Runtime policy and execution remain in saya-core. Callers supply UI adapters.
pub mod context;
pub mod events;
pub mod export;
pub mod interaction;
pub mod session;
use saya_core::{
    approvals::Preauthorizations,
    boundary::{Cancellation, Model, Store},
    config::{Auth, Config, Output, Profile, Protocol},
    contract::{BoundaryError, ModelProtocol, PermissionMode, Run, RunId, Session, SessionId},
    input::InputHandler,
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
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};
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
        thinking_effort: p.thinking_effort,
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
/// Per-run application services. The caller owns confirmation and input adapters.
/// Cancellation, approvals and store lifetime apply to the complete run.
pub struct Launch {
    pub permissions: PermissionMode,
    pub store: Arc<dyn Store>,
    pub cancellation: Arc<dyn Cancellation>,
    pub confirmation: Option<Arc<dyn ApprovalHandler>>,
    pub input: Option<Arc<dyn InputHandler>>,
    pub approvals: Preauthorizations,
    pub personal_context: PersonalContext,
    pub anthropic_prompt_cache: bool,
    pub run_id: Option<RunId>,
}
/// Build empty session metadata from the selected profile and canonical workspace.
/// Returns an error for missing configuration or an unavailable workspace.
pub fn new_session(config: &Config) -> Result<Session, Box<dyn std::error::Error>> {
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
/// Assemble a policy-checked runtime and decorate its model with interface events.
/// Preparation may read project instructions. Execution begins with runtime.run.
pub fn build_runtime(
    config: &Config,
    session: &Session,
    summary: &str,
    launch: Launch,
    decorate: impl FnOnce(Arc<dyn Model>, bool) -> Arc<dyn Model>,
) -> Result<(AgentRuntime, bool), Box<dyn std::error::Error>> {
    let profile = &session.profile;
    let selected = config.profiles.get(profile).ok_or("profile not found")?;
    let run = Run {
        id: launch.run_id.unwrap_or_default(),
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
    let secrets = config.secret_env_names();
    let known_secrets = secrets
        .iter()
        .map(|name| config.secret(name).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    let streamed = selected.stream && config.output == Output::Text && known_secrets.is_empty();
    let model = decorate(router, streamed);
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
        write_max_bytes: lim.write_max_bytes,
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
    let review_profile = config.review.profile.as_ref().unwrap_or(profile);
    runtime = runtime.with_pricing(
        selected.pricing.clone(),
        config
            .profiles
            .get(review_profile)
            .and_then(|profile| profile.pricing.clone()),
    );
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
