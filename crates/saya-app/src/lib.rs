//! Shared application assembly and durable session lifecycle for Saya interfaces.
//! Runtime policy and execution remain in saya-core. Callers supply UI adapters.
pub mod context;
pub mod events;
pub mod export;
mod extensions;
pub mod interaction;
pub mod mcp;
pub mod memory_admin;
pub mod session;
pub mod thinking;
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
    let t = Arc::new(HttpTransport::new_with_environment(
        HttpConfig {
            base_url: p.base_url.clone(),
            allow_remote_http: p.allow_remote_http,
            authentication: auth,
            headers_env: p.headers_env.clone(),
            connect_timeout: Duration::from_secs(config.limits.model_connect_timeout_seconds),
            idle_timeout: Duration::from_secs(config.limits.model_idle_timeout_seconds),
            total_timeout: Duration::from_secs(config.limits.model_timeout_seconds),
        },
        config.environment(),
    )?);
    let settings = ModelSettings {
        profile: name.into(),
        model: p.model.clone(),
        context_window_tokens: p.context_window_tokens,
        max_output_tokens: p.max_output_tokens,
        multimodal: p.multimodal,
        upstream_max_output_tokens: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        extra_safety_tokens: config.context.extra_safety_tokens,
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
    pub secret_registry: Option<Arc<saya_core::credentials::SecretRegistry>>,
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
    let registry = launch.secret_registry.unwrap_or_default();
    registry.register(&known_secrets)?;
    let plugin_store = extensions::selected_store(config, session, registry.clone())?;
    let dynamic_credentials =
        plugin_store.is_some() || config.mcp.servers.values().any(|server| server.enabled);
    let streamed = selected.stream
        && config.output == Output::Text
        && known_secrets.is_empty()
        && !dynamic_credentials;
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
        execution: None,
        environment: config.environment().clone(),
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
        limits: limits.clone(),
    })?;
    let compact = saya_core::compaction::CompactionConfig {
        summary_output_tokens: config
            .context
            .summary_max_output_tokens
            .unwrap_or(selected.max_output_tokens),
        max_model_calls: config.context.max_compaction_calls,
        connect_timeout: limits.model_connect.min(Duration::from_secs(3600)),
        idle_timeout: limits.model_idle.min(Duration::from_secs(3600)),
        request_timeout: limits.model_total.min(Duration::from_secs(3600)),
        operation_timeout: Duration::from_secs(config.context.compaction_timeout_seconds)
            .min(limits.active_time),
        retry_jitter_max: limits.retry_jitter_max,
        ..Default::default()
    };
    runtime = runtime.with_context_settings(
        config.context.budget(selected),
        config.context.auto_compact,
        compact,
    )?;
    let (mut sandbox, broker) = config.sandbox.resolve(&session.workspace)?;
    sandbox
        .read_roots
        .extend(config.permissions.additional_roots.iter().cloned());
    sandbox
        .write_roots
        .extend(config.permissions.additional_roots.iter().cloned());
    let credential_path = config.state_dir.join("credentials");
    let credential_backed = config.mcp.servers.values().any(|server| {
        server.enabled
            && matches!(
                &server.transport,
                saya_core::mcp::config::McpTransportSettings::Http(settings)
                    if settings.credential_ref.is_some()
            )
    });
    if sandbox.backend != saya_core::execution::BackendKind::Host {
        for path in [
            config.config_path.clone(),
            config.data_dir.clone(),
            config.state_dir.clone(),
            config.cache_dir.clone(),
        ] {
            if path.try_exists()? {
                sandbox.protected_paths.push(path);
            }
        }
        let project_control = session.workspace.join(".saya/local");
        if project_control.try_exists()? {
            sandbox.protected_paths.push(project_control);
        }
    } else if credential_backed || credential_path.exists() {
        return Err("host execution cannot protect the credential store".into());
    }
    let authorizer: Option<Arc<dyn saya_core::mcp::config::McpHttpAuthorizer>> =
        if credential_backed {
            std::fs::create_dir_all(&config.state_dir)?;
            let credentials = saya_core::credentials::FileCredentialStore::open(
                &credential_path,
                registry.clone(),
            )?;
            sandbox
                .protected_paths
                .push(credentials.protected_path().to_owned());
            Some(Arc::new(mcp::StoredMcpAuthorizer::new(credentials)))
        } else {
            None
        };
    if config.output_capture.enabled {
        let outputs = Arc::new(saya_core::output_store::OutputStore::open(
            config.data_dir.join("outputs"),
            config.output_capture.limits,
            registry.clone(),
        )?);
        if sandbox.backend != saya_core::execution::BackendKind::Host {
            sandbox
                .protected_paths
                .push(outputs.protected_path().to_owned());
        }
        runtime = runtime.with_output_store(outputs)?;
    }
    let memory_root = config.memory_root()?;
    let memory = Arc::new(saya_core::global_memory::GlobalMemoryStore::open(
        &memory_root,
        saya_core::global_memory::WorkspaceIdentity::from_path(&session.workspace)?,
        saya_core::global_memory::MemoryLimits::default(),
        registry.clone(),
    )?);
    let memory_paths = vec![
        memory_root.join("MEMORY.md"),
        memory_root.join("memory"),
        memory_root.join(".saya-memory.lock"),
    ];
    if sandbox.backend != saya_core::execution::BackendKind::Host {
        sandbox.protected_paths.extend(memory_paths.iter().cloned());
    }
    runtime = runtime.with_global_memory(memory, config.memory.clone())?;
    let mut private_paths = vec![
        config.config_path.clone(),
        config.data_dir.clone(),
        config.state_dir.clone(),
        config.cache_dir.clone(),
        session.workspace.join(".saya/local"),
    ];
    private_paths.extend(memory_paths);
    private_paths.extend(sandbox.protected_paths.iter().cloned());
    runtime = runtime.with_private_paths(private_paths.clone())?;
    let checkpoints = Arc::new(saya_core::checkpoint::CheckpointStore::open(
        config.data_dir.join("checkpoints"),
        registry.clone(),
        saya_core::checkpoint::CheckpointLimits::default(),
    )?);
    private_paths.push(checkpoints.protected_path().to_owned());
    if sandbox.backend != saya_core::execution::BackendKind::Host {
        sandbox
            .protected_paths
            .push(checkpoints.protected_path().to_owned());
    }
    runtime = runtime.with_checkpoints(checkpoints, private_paths, sandbox.write_roots.clone())?;
    if let Some(store) = plugin_store {
        runtime = extensions::attach(
            runtime,
            config,
            session,
            &mut sandbox,
            store,
            registry.clone(),
        )?;
    }
    runtime = runtime.with_execution_settings(sandbox, broker)?;
    if dynamic_credentials {
        runtime = runtime.with_mcp_settings(
            config.mcp.clone(),
            config.environment().clone(),
            authorizer,
            registry.clone(),
        )?;
    } else {
        runtime = runtime.with_secret_registry(registry.clone())?;
    }
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
    runtime = runtime.with_skill_manager(saya_core::skills::SkillManager::new(
        &session.workspace,
        home.as_deref(),
        registry,
        saya_core::skills::SkillLimits::default(),
    )?)?;
    if let Some(input) = launch.input {
        runtime = runtime.with_input_handler(input);
    }
    runtime = runtime.with_personal_context(launch.personal_context);
    Ok((runtime, streamed))
}
