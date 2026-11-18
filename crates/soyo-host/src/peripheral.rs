//! Peripheral Tauri commands and runtime helpers (settings, extensions, modules, MCP/credentials, memory, export, attachments).

use crate::{
    attachment, envfile,
    service::{HostService, register_config_secrets},
};
use soyo_app::{
    interaction::RunCancellation, memory_admin::MemoryAdmin, offload_boundary, session,
};
use soyo_core::{
    background::JobStatus,
    config::{self, Config, Environment, Overrides, ThinkingEffort},
    contract::{BoundaryError, SessionId, error_codes},
    credentials::{
        CredentialKind, CredentialMetadata, CredentialRef, CredentialScope, CredentialValue,
        FileCredentialStore, SecretValue,
    },
    execution::BackendKind,
    export::{ExportOptions, ExportScope, SessionExport},
    mcp::config::McpTransportSettings,
    modules::{ModuleKind, ModuleStateSource, ModuleStatus, ModuleStore, statuses},
    plugin_store::{PluginStore, StoreLimits},
    project_context::ProjectContext,
    sqlite::{SnapshotLimits, SqliteStore},
};
use soyo_ipc::{
    redaction,
    types::{
        BackgroundJobDto, BootstrapStateDto, CredentialRecordDto, DesktopError, ExportFormatDto,
        ExportResultDto, ExportScopeDto, ExtensionItemDto, McpServerDto, MemoryConfirmResultDto,
        MemoryQueryResultDto, ModuleStatusDto, PermissionModeDto, PreparedMemoryReviewDto,
        SandboxBackendDto, SetupEnvironmentParams, StagedAttachmentDto, WorkModeDto,
    },
};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};
use uuid::Uuid;

fn backend_kind_from_dto(value: &SandboxBackendDto) -> BackendKind {
    match value {
        SandboxBackendDto::Auto => BackendKind::Auto,
        SandboxBackendDto::Seatbelt => BackendKind::Seatbelt,
        SandboxBackendDto::Bubblewrap => BackendKind::Bubblewrap,
        SandboxBackendDto::Host => BackendKind::Host,
    }
}

async fn require_config_clone(state: &HostService) -> Result<Config, DesktopError> {
    let guard = state.inner.lock().await;
    let err_msg = guard
        .config_error
        .clone()
        .unwrap_or_else(|| "Configuration is not loaded".into());
    guard
        .config
        .clone()
        .ok_or_else(|| DesktopError::new("invalid_config", err_msg))
}

async fn require_idle_controls(state: &HostService) -> Result<(), DesktopError> {
    let controls = state
        .active_controls
        .lock()
        .map_err(|_| DesktopError::new("state_lock", "active control lock failed"))?;
    if controls.values().any(|active| active.pending.is_some()) {
        return Err(DesktopError::new(
            "pending_request",
            "Resolve pending approval or input first",
        ));
    }
    if !controls.is_empty() {
        return Err(DesktopError::new(
            "run_active",
            "Cancel or wait for active runs first",
        ));
    }
    Ok(())
}

async fn require_idle_config_clone(state: &HostService) -> Result<Config, DesktopError> {
    require_idle_controls(state).await?;
    let guard = state.inner.lock().await;
    if guard.sessions.values().any(|s| s.active_run_id.is_some()) {
        return Err(DesktopError::new(
            "run_active",
            "Cancel or wait for active runs first",
        ));
    }
    let err_msg = guard
        .config_error
        .clone()
        .unwrap_or_else(|| "Configuration is not loaded".into());
    guard
        .config
        .clone()
        .ok_or_else(|| DesktopError::new("invalid_config", err_msg))
}

fn map_module_status(status: ModuleStatus, secrets: &[String]) -> ModuleStatusDto {
    ModuleStatusDto {
        module: status.module.as_str().to_owned(),
        description: status.description.to_owned(),
        enabled: status.enabled,
        configured_enabled: status.configured_enabled,
        override_enabled: status.override_enabled,
        source: match status.source {
            ModuleStateSource::Config => "config".into(),
            ModuleStateSource::ModuleStore => "module_store".into(),
        },
        active: status.active,
        issue: status.issue.map(|msg| redaction::redact_str(&msg, secrets)),
    }
}

fn map_credential_metadata(meta: CredentialMetadata, secrets: &[String]) -> CredentialRecordDto {
    CredentialRecordDto {
        reference: meta.reference.to_string(),
        server: redaction::redact_str(&meta.scope.server_id, secrets),
        audience: redaction::redact_str(&meta.scope.audience, secrets),
        scopes: meta
            .scope
            .scopes
            .into_iter()
            .map(|s| redaction::redact_str(&s, secrets))
            .collect(),
        kind: match meta.kind {
            CredentialKind::Secret => "secret".into(),
            CredentialKind::OAuth => "oauth".into(),
        },
        revision: meta.revision,
        revoked: meta.revoked,
        oauth_resource: meta
            .oauth_resource
            .map(|r| redaction::redact_str(&r, secrets)),
    }
}

fn build_mcp_list(cfg: &Config, secrets: &[String]) -> Vec<McpServerDto> {
    cfg.mcp
        .servers
        .iter()
        .map(|(name, server)| match &server.transport {
            McpTransportSettings::Stdio(stdio) => McpServerDto {
                name: redaction::redact_str(name, secrets),
                transport: "stdio".into(),
                enabled: server.enabled,
                target: redaction::redact_str(&stdio.argv.join(" "), secrets),
                scopes: Vec::new(),
                credential_ref: None,
            },
            McpTransportSettings::Http(http) => McpServerDto {
                name: redaction::redact_str(name, secrets),
                transport: "http".into(),
                enabled: server.enabled,
                target: redaction::redact_str(&http.endpoint, secrets),
                scopes: http
                    .scopes
                    .iter()
                    .map(|s| redaction::redact_str(s, secrets))
                    .collect(),
                credential_ref: http.credential_ref.map(|r| r.to_string()),
            },
        })
        .collect()
}

fn parse_effort(raw: Option<&str>) -> Result<Option<ThinkingEffort>, DesktopError> {
    let Some(value) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    match value.to_ascii_lowercase().as_str() {
        "default" => Ok(None),
        "low" => Ok(Some(ThinkingEffort::Low)),
        "medium" => Ok(Some(ThinkingEffort::Medium)),
        "high" => Ok(Some(ThinkingEffort::High)),
        "xhigh" => Ok(Some(ThinkingEffort::XHigh)),
        "max" => Ok(Some(ThinkingEffort::Max)),
        other => Err(DesktopError::new(
            "invalid_args",
            format!("Unsupported thinking effort '{other}'"),
        )),
    }
}

async fn open_credential_store(
    state_dir: &Path,
    registry: std::sync::Arc<soyo_core::credentials::SecretRegistry>,
    secrets: &[String],
) -> Result<FileCredentialStore, DesktopError> {
    let state_dir = state_dir.to_path_buf();
    let cred_dir = state_dir.join("credentials");
    offload_boundary(move || {
        soyo_fs::ensure_root_dir(&state_dir, "credential_io", "credential state")?;
        FileCredentialStore::open(&cred_dir, registry)
    })
    .await
    .map_err(|err| DesktopError::from_boundary(&err, secrets))
}

impl HostService {
    pub async fn set_workspace(&self, path: &str) -> Result<BootstrapStateDto, DesktopError> {
        self.set_workspace_for(0, None, path).await
    }

    pub async fn set_workspace_for(
        &self,
        conn_id: u64,
        session_id: Option<&str>,
        path: &str,
    ) -> Result<BootstrapStateDto, DesktopError> {
        let cfg = require_config_clone(self).await?;
        let secrets = self.current_secrets();
        let trimmed = path.trim();
        if trimmed.is_empty() {
            return Err(DesktopError::new(
                "invalid_args",
                "Workspace path cannot be empty",
            ));
        }

        let requested = PathBuf::from(trimmed);
        let base_cwd = cfg.cwd.clone();
        let cfg_for_check = cfg.clone();
        let workspace = offload_boundary(move || {
            let candidate = if requested.is_absolute() {
                requested
            } else {
                base_cwd.join(requested)
            };
            let canonical = std::fs::canonicalize(&candidate).map_err(|_| {
                BoundaryError::new("workspace_missing", "Workspace directory not found")
            })?;
            if !canonical.is_dir() {
                return Err(BoundaryError::new(
                    "workspace_not_directory",
                    "Workspace target is not a directory",
                ));
            }
            session::check_roots(&cfg_for_check, &canonical, cfg_for_check.mode)
                .map_err(|err| BoundaryError::new("invalid_workspace_roots", err.to_string()))?;
            Ok(canonical)
        })
        .await
        .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;

        {
            let mut guard = self.inner.lock().await;
            let target_sid = guard.resolve_session_id(session_id, conn_id);
            if let Some(sid) = target_sid {
                let profile_lookup = {
                    let slot = guard.sessions.get(&sid);
                    guard.config.as_ref().and_then(|c| {
                        slot.and_then(|s| c.profiles.get(&s.prepared.session.profile))
                    })
                }
                .cloned();
                if let Some(slot) = guard.sessions.get_mut(&sid) {
                    if slot.active_run_id.is_some() {
                        return Err(DesktopError::new(
                            "run_active",
                            "Cancel or wait for the active run before changing workspace",
                        ));
                    }
                    if slot.owner_conn_id.is_some_and(|o| o != conn_id) {
                        return Err(DesktopError::new(
                            "unauthorized_client",
                            "Session belongs to another client connection",
                        ));
                    }
                    let Some(profile) = profile_lookup else {
                        return Err(DesktopError::new(
                            "invalid_config",
                            "Session profile not found in configuration",
                        ));
                    };
                    let fingerprint = profile
                        .fingerprint()
                        .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
                    let mut updated = slot.prepared.session.clone();
                    updated.workspace = workspace;
                    slot.prepared
                        .store
                        .update_session_config(&updated, &fingerprint)
                        .await
                        .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
                    slot.prepared.session = updated;
                    drop(guard);
                    return Ok(self.get_bootstrap_state_for(conn_id, session_id).await);
                }
            }
            if let Some(cfg_mut) = &mut guard.config {
                cfg_mut.cwd = workspace;
            }
        }

        Ok(self.get_bootstrap_state_for(conn_id, session_id).await)
    }

    pub async fn select_profile(&self, profile: &str) -> Result<BootstrapStateDto, DesktopError> {
        self.select_profile_for(0, None, profile).await
    }

    pub async fn select_profile_for(
        &self,
        conn_id: u64,
        session_id: Option<&str>,
        profile: &str,
    ) -> Result<BootstrapStateDto, DesktopError> {
        let mut guard = self.inner.lock().await;
        let target_sid = guard.resolve_session_id(session_id, conn_id);
        if let Some(sid) = target_sid
            && let Some(slot) = guard.sessions.get(&sid)
        {
            if slot.active_run_id.is_some() {
                return Err(DesktopError::new(
                    "run_active",
                    "Cancel or wait for the active run before changing profile",
                ));
            }
            if slot.prepared.session.profile != profile {
                return Err(DesktopError::new(
                    "session_profile_locked",
                    "Start a new session before switching to a different model profile",
                ));
            }
        }
        let cfg = guard
            .config
            .as_mut()
            .ok_or_else(|| DesktopError::new("invalid_config", "Configuration is not loaded"))?;
        if !cfg.profiles.contains_key(profile) {
            return Err(DesktopError::new(
                "invalid_profile",
                format!("Profile '{profile}' is not defined"),
            ));
        }
        cfg.profile = Some(profile.to_owned());
        drop(guard);
        Ok(self.get_bootstrap_state_for(conn_id, session_id).await)
    }

    /// First-run setup: validate inputs, store the API key value in
    /// `<state_dir>/secrets/env` (`0700`/`0600`), merge the profile into `config.toml`,
    /// reload configuration, then apply the optional workspace.
    pub async fn setup_environment(
        &self,
        params: SetupEnvironmentParams,
    ) -> Result<BootstrapStateDto, DesktopError> {
        self.setup_environment_for(0, params).await
    }

    pub async fn setup_environment_for(
        &self,
        conn_id: u64,
        params: SetupEnvironmentParams,
    ) -> Result<BootstrapStateDto, DesktopError> {
        let secrets = self.current_secrets();
        require_idle_controls(self).await?;
        let target = {
            let guard = self.inner.lock().await;
            if guard.sessions.values().any(|s| s.active_run_id.is_some()) {
                return Err(DesktopError::new(
                    "run_active",
                    "Cancel or wait for active runs before running setup",
                ));
            }
            guard.target.clone()
        };

        let name = params.profile_name.trim();
        if name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(DesktopError::new(
                "invalid_args",
                "Profile name must be 1-64 letters, digits, '_' or '-'",
            ));
        }
        let protocol = match params.protocol.trim() {
            p @ ("openai-responses" | "openai-chat-completions" | "anthropic-messages") => p,
            _ => return Err(DesktopError::new("invalid_args", "Unsupported protocol")),
        };
        let auth = match params.auth.as_deref().unwrap_or("bearer").trim() {
            a @ ("bearer" | "x-api-key" | "none") => a,
            _ => return Err(DesktopError::new("invalid_args", "Unsupported auth scheme")),
        };
        let base_url = params.base_url.trim();
        let model = params.model.trim();
        if base_url.is_empty() || model.is_empty() {
            return Err(DesktopError::new(
                "invalid_args",
                "Base URL and model are required",
            ));
        }
        let context_window = params.context_window_tokens.unwrap_or(128_000);
        let max_output = params.max_output_tokens.unwrap_or(8_192);
        if context_window == 0 || max_output == 0 {
            return Err(DesktopError::new(
                "invalid_args",
                "Token limits must be positive",
            ));
        }
        let key_env = params
            .api_key_env
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let key_value = params
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        if auth == "none" {
            if key_env.is_some() || key_value.is_some() {
                return Err(DesktopError::new(
                    "invalid_args",
                    "auth 'none' cannot reference an API key",
                ));
            }
        } else {
            let Some(env_name) = key_env else {
                return Err(DesktopError::new(
                    "invalid_args",
                    "An API key variable name is required for keyed auth",
                ));
            };
            if !soyo_core::config::valid_env_name(env_name) {
                return Err(DesktopError::new(
                    "invalid_args",
                    "Invalid environment variable name",
                ));
            }
        }

        if let (Some(env_name), Some(value)) = (key_env, key_value.as_ref()) {
            let state_dir = target.state_dir.clone();
            let env_name = env_name.to_owned();
            let value_owned = value.clone();
            offload_boundary(move || {
                envfile::upsert_env_pair_for(&state_dir, &env_name, &value_owned)
            })
            .await
            .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
            let _ = self.secrets.register(std::slice::from_ref(value));
        }

        let merge = MergeSetup {
            name: name.to_owned(),
            protocol: protocol.to_owned(),
            base_url: base_url.to_owned(),
            model: model.to_owned(),
            auth: auth.to_owned(),
            key_env: key_env.map(str::to_owned),
            context_window,
            max_output,
            permissions: params.permission_mode.map(permission_label),
            mode: params.work_mode.map(work_mode_label),
        };
        let target_cfg_path = target.config_path.clone();
        let (config_path, backup_path) =
            offload_boundary(move || merge_setup_config(&target_cfg_path, &merge))
                .await
                .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;

        let overlay_pairs = envfile::load_env_pairs_for(&target.state_dir);
        let overlay_values = envfile::load_env_secret_values_for(&target.state_dir);
        let _ = self.secrets.register(&overlay_values);
        let env = Environment::capture_with(overlay_pairs);
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let reload_overrides = Overrides {
            config: Some(target.config_path.clone()),
            data_dir: Some(target.data_dir.clone()),
            ..Default::default()
        };
        let reload = offload_boundary(move || config::load(reload_overrides, env, &cwd))
            .await
            .map_err(|err| DesktopError::from_boundary(&err, &secrets));
        let cfg = match reload {
            Ok(cfg) => cfg,
            Err(err) => {
                restore_config_backup(&config_path, backup_path);
                return Err(err);
            }
        };
        {
            let mut guard = self.inner.lock().await;
            guard.target = soyo_ipc::socket::HelperTarget::from_config(&cfg);
            guard.config = Some(cfg.clone());
            guard.config_error = None;
            register_config_secrets(&cfg, &self.secrets);
        }

        let workspace = params
            .workspace
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        if let Some(path) = workspace {
            // First-run setup creates the directory when it does not exist;
            // `set_workspace` alone only accepts pre-existing directories.
            let base_cwd = {
                let guard = self.inner.lock().await;
                guard.config.as_ref().map(|cfg| cfg.cwd.clone())
            };
            let requested = PathBuf::from(&path);
            let resolved = offload_boundary(move || -> Result<PathBuf, BoundaryError> {
                let candidate = match (requested.is_absolute(), &base_cwd) {
                    (true, _) => requested,
                    (false, Some(base)) => base.join(requested),
                    (false, None) => requested,
                };
                if !candidate.exists() {
                    fs::create_dir_all(&candidate).map_err(|_| {
                        BoundaryError::new("workspace_missing", "Cannot create workspace directory")
                    })?;
                }
                Ok(candidate)
            })
            .await
            .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
            return self
                .set_workspace_for(conn_id, None, &resolved.to_string_lossy())
                .await;
        }
        Ok(self.get_bootstrap_state_for(conn_id, None).await)
    }

    pub async fn set_thinking_effort(
        &self,
        effort: Option<String>,
    ) -> Result<BootstrapStateDto, DesktopError> {
        self.set_thinking_effort_for(0, None, effort).await
    }

    pub async fn set_thinking_effort_for(
        &self,
        conn_id: u64,
        session_id: Option<&str>,
        effort: Option<String>,
    ) -> Result<BootstrapStateDto, DesktopError> {
        let _ = require_idle_config_clone(self).await?;
        let parsed = parse_effort(effort.as_deref())?;
        let secrets = self.current_secrets();
        let mut guard = self.inner.lock().await;
        let target_sid = guard.resolve_session_id(session_id, conn_id);
        let session_profile = target_sid
            .and_then(|sid| guard.sessions.get(&sid))
            .map(|s| s.prepared.session.profile.clone());
        let (profile_name, previous, fingerprint) = {
            let cfg = guard.config.as_mut().ok_or_else(|| {
                DesktopError::new("invalid_config", "Configuration is not loaded")
            })?;
            let Some(profile_name) = session_profile.or_else(|| cfg.profile.clone()) else {
                return Err(DesktopError::new(
                    "invalid_config",
                    "Select a configured profile first",
                ));
            };
            let profile = cfg.profiles.get_mut(&profile_name).ok_or_else(|| {
                DesktopError::new("invalid_config", "Selected profile is missing")
            })?;
            let previous = profile.thinking_effort;
            profile.thinking_effort = parsed;
            let fingerprint = profile
                .fingerprint()
                .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
            (profile_name, previous, fingerprint)
        };
        if let Some(sid) = target_sid
            && let Some(slot) = guard.sessions.get_mut(&sid)
            && let Err(err) = slot
                .prepared
                .store
                .update_session_config(&slot.prepared.session, &fingerprint)
                .await
        {
            if let Some(cfg) = &mut guard.config
                && let Some(profile) = cfg.profiles.get_mut(&profile_name)
            {
                profile.thinking_effort = previous;
            }
            return Err(DesktopError::from_boundary(&err, &secrets));
        }
        drop(guard);
        Ok(self.get_bootstrap_state_for(conn_id, session_id).await)
    }

    pub async fn set_sandbox_backend(
        &self,
        backend: SandboxBackendDto,
    ) -> Result<BootstrapStateDto, DesktopError> {
        let _ = require_idle_config_clone(self).await?;
        let mut guard = self.inner.lock().await;
        let cfg = guard
            .config
            .as_mut()
            .ok_or_else(|| DesktopError::new("invalid_config", "Configuration is not loaded"))?;
        cfg.sandbox.backend = backend_kind_from_dto(&backend);
        drop(guard);
        Ok(self.get_bootstrap_state().await)
    }

    /// Toggle one managed-memory setting (`set_memory_setting`).
    pub async fn set_memory_setting(
        &self,
        setting: &str,
        enabled: bool,
    ) -> Result<(), DesktopError> {
        let _ = require_idle_config_clone(self).await?;
        let mut guard = self.inner.lock().await;
        let cfg = guard
            .config
            .as_mut()
            .ok_or_else(|| DesktopError::new("invalid_config", "Configuration is not loaded"))?;
        match setting {
            "read" => cfg.memory.read = enabled,
            "inject" => cfg.memory.inject = enabled,
            "write" => cfg.memory.write = enabled,
            _ => {
                return Err(DesktopError::new(
                    "invalid_args",
                    "setting must be read, inject or write",
                ));
            }
        }
        Ok(())
    }

    pub async fn set_autocompact(
        &self,
        enabled: bool,
        trigger_tokens: Option<u64>,
    ) -> Result<BootstrapStateDto, DesktopError> {
        let _ = require_idle_config_clone(self).await?;
        let secrets = self.current_secrets();
        let mut guard = self.inner.lock().await;
        let cfg = guard
            .config
            .as_mut()
            .ok_or_else(|| DesktopError::new("invalid_config", "Configuration is not loaded"))?;

        if !enabled {
            cfg.context.auto_compact = false;
        } else if let Some(tokens) = trigger_tokens {
            if tokens == 0 {
                return Err(DesktopError::new(
                    "invalid_args",
                    "Auto-compact trigger tokens must be positive",
                ));
            }
            let Some(profile_name) = cfg.profile.as_ref() else {
                return Err(DesktopError::new(
                    "invalid_config",
                    "Select a configured profile first",
                ));
            };
            let profile = cfg.profiles.get(profile_name).ok_or_else(|| {
                DesktopError::new("invalid_config", "Selected profile is missing")
            })?;
            let budget = cfg.context.budget(profile);
            let capacity = budget
                .input_capacity()
                .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
            let target =
                ((u128::from(capacity) * u128::from(cfg.context.target_percent)) / 100) as u64;
            let effective = tokens.min(capacity);
            if effective <= target {
                return Err(DesktopError::new(
                    "invalid_args",
                    format!("Auto-compact trigger must exceed target threshold ({target} tokens)"),
                ));
            }
            cfg.context.auto_compact = true;
            cfg.context.trigger_tokens = Some(effective);
        } else {
            cfg.context.auto_compact = true;
            cfg.context.trigger_tokens = None;
        }

        drop(guard);
        Ok(self.get_bootstrap_state().await)
    }

    pub async fn list_extensions(&self) -> Result<Vec<ExtensionItemDto>, DesktopError> {
        let cfg = require_config_clone(self).await?;
        let secrets = self.current_secrets();
        let registry = self.secrets.clone();
        let items = offload_boundary(move || {
            let project = ProjectContext::new(&cfg.cwd, cfg.home().as_deref())?;
            let snapshot = project.snapshot(None)?;
            let mut result = Vec::new();

            for inst in snapshot.instructions {
                let name = Path::new(&inst.path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| inst.path.to_string_lossy().into_owned());
                result.push(ExtensionItemDto {
                    kind: "rule".into(),
                    id: inst.sha256,
                    name,
                    description: inst.path.to_string_lossy().into_owned(),
                    scope: inst.scope.into(),
                    version: None,
                    active: true,
                });
            }

            for skill in snapshot.skills {
                result.push(ExtensionItemDto {
                    kind: "skill".into(),
                    id: skill.sha256,
                    name: skill.name,
                    description: skill.description,
                    scope: skill.scope.into(),
                    version: None,
                    active: true,
                });
            }

            let plugin_root = cfg.data_dir.join("plugins");
            if cfg.plugins.enabled && plugin_root.try_exists().unwrap_or(false) {
                session::check_roots(&cfg, &cfg.cwd, cfg.mode)
                    .map_err(|err| BoundaryError::new("plan_read_only", err.to_string()))?;
                let store = PluginStore::open(&plugin_root, registry, StoreLimits::default())?;
                for summary in store.list()? {
                    let active = summary.enabled_scopes.iter().any(|r| r.enabled);
                    result.push(ExtensionItemDto {
                        kind: "plugin".into(),
                        id: summary.digest.clone(),
                        name: summary.plugin_id,
                        description: format!(
                            "{:?} · {} scope(s)",
                            summary.state,
                            summary.enabled_scopes.len()
                        ),
                        scope: "plugin_store".into(),
                        version: Some(summary.version),
                        active,
                    });
                }
            }
            Ok(result)
        })
        .await
        .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;

        Ok(items
            .into_iter()
            .map(|item| ExtensionItemDto {
                kind: item.kind,
                id: item.id,
                name: redaction::redact_str(&item.name, &secrets),
                description: redaction::redact_str(&item.description, &secrets),
                scope: redaction::redact_str(&item.scope, &secrets),
                version: item.version.map(|v| redaction::redact_str(&v, &secrets)),
                active: item.active,
            })
            .collect())
    }

    pub async fn list_jobs(&self) -> Result<Vec<BackgroundJobDto>, DesktopError> {
        self.list_jobs_for(0, None).await
    }

    pub async fn list_jobs_for(
        &self,
        conn_id: u64,
        session_id: Option<&str>,
    ) -> Result<Vec<BackgroundJobDto>, DesktopError> {
        let secrets = self.current_secrets();
        let (store, sid, active_run) = {
            let guard = self.inner.lock().await;
            let Some(sid) = guard.resolve_session_id(session_id, conn_id) else {
                return Ok(Vec::new());
            };
            let Some(slot) = guard.sessions.get(&sid) else {
                return Ok(Vec::new());
            };
            (slot.prepared.store.clone(), sid, slot.active_run_id)
        };
        let jobs = store
            .background_jobs(sid)
            .await
            .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
        let active_handle = active_run.and_then(|run_id| {
            self.active_controls
                .lock()
                .ok()
                .and_then(|controls| controls.get(&run_id).and_then(|c| c.handle.clone()))
        });

        Ok(jobs
            .into_iter()
            .map(|job| {
                let mut state = job.state;
                if let Some(handle) = &active_handle
                    && let Ok(Some(live)) = handle.background_status(&job.job_id)
                {
                    state = match live {
                        JobStatus::Running => "running".into(),
                        JobStatus::Finished(res) => {
                            if res.ok {
                                "succeeded".into()
                            } else {
                                "failed".into()
                            }
                        }
                        JobStatus::Failed(_) => "failed".into(),
                    };
                }
                let ok = job.result.as_ref().map(|r| r.ok);
                let summary = job
                    .result
                    .as_ref()
                    .and_then(|r| {
                        r.error
                            .as_ref()
                            .map(|e| e.message.clone())
                            .or_else(|| r.data.as_ref().map(|v| v.to_string()))
                    })
                    .map(|s| redaction::redact_str(&s, &secrets));
                BackgroundJobDto {
                    job_id: job.job_id,
                    run_id: job.run_id.0.to_string(),
                    parent_call_id: job.parent_call_id,
                    tool: job.tool.map(|t| redaction::redact_str(&t, &secrets)),
                    state: redaction::redact_str(&state, &secrets),
                    ok,
                    summary,
                }
            })
            .collect())
    }

    /// Single background job by id (`get_job`), with live-status overlay.
    pub async fn get_job(&self, job_id: &str) -> Result<Option<BackgroundJobDto>, DesktopError> {
        self.get_job_for(0, None, job_id).await
    }

    pub async fn get_job_for(
        &self,
        conn_id: u64,
        session_id: Option<&str>,
        job_id: &str,
    ) -> Result<Option<BackgroundJobDto>, DesktopError> {
        let jobs = self.list_jobs_for(conn_id, session_id).await?;
        Ok(jobs.into_iter().find(|job| job.job_id == job_id))
    }

    /// Request cancellation of a still-running background job (`cancel_job`).
    pub async fn cancel_job(&self, job_id: &str) -> Result<bool, DesktopError> {
        self.cancel_job_for(0, job_id).await
    }

    pub async fn cancel_job_for(&self, conn_id: u64, job_id: &str) -> Result<bool, DesktopError> {
        let secrets = self.current_secrets();
        let handles: Vec<_> = self
            .active_controls
            .lock()
            .ok()
            .map(|controls| {
                controls
                    .values()
                    .filter(|c| c.owner_conn_id == conn_id)
                    .filter_map(|c| c.handle.clone())
                    .collect()
            })
            .unwrap_or_default();
        for handle in handles {
            if let Some(JobStatus::Running) = handle
                .background_status(job_id)
                .map_err(|err| DesktopError::from_boundary(&err, &secrets))?
            {
                handle
                    .cancel_background(job_id)
                    .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn cancel_background_job(&self, job_id: &str) -> Result<bool, DesktopError> {
        self.cancel_background_job_for(0, job_id)
    }

    pub fn cancel_background_job_for(
        &self,
        conn_id: u64,
        job_id: &str,
    ) -> Result<bool, DesktopError> {
        let secrets = self.current_secrets();
        let controls = self
            .active_controls
            .lock()
            .map_err(|_| DesktopError::new("state_lock", "active control lock failed"))?;
        let owned: Vec<_> = controls
            .values()
            .filter(|c| c.owner_conn_id == conn_id)
            .collect();
        if owned.is_empty() {
            return Err(DesktopError::new(
                "no_active_run",
                "Background job cancellation requires an active run",
            ));
        }
        let mut last_err = None;
        for control in owned {
            let Some(handle) = control.handle.as_ref() else {
                continue;
            };
            match handle.background_status(job_id) {
                Ok(Some(_)) => {
                    return handle
                        .cancel_background(job_id)
                        .map_err(|err| DesktopError::from_boundary(&err, &secrets));
                }
                Ok(None) => {}
                Err(err) => last_err = Some(err),
            }
        }
        if let Some(err) = last_err {
            return Err(DesktopError::from_boundary(&err, &secrets));
        }
        Ok(false)
    }

    pub async fn list_modules(&self) -> Result<Vec<ModuleStatusDto>, DesktopError> {
        let cfg = require_config_clone(self).await?;
        let secrets = self.current_secrets();
        let list = offload_boundary(move || statuses(&cfg.data_dir, &cfg.browser))
            .await
            .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
        Ok(list
            .into_iter()
            .map(|status| map_module_status(status, &secrets))
            .collect())
    }

    pub async fn set_module_enabled(
        &self,
        module: &str,
        enabled: bool,
    ) -> Result<Vec<ModuleStatusDto>, DesktopError> {
        let cfg = require_idle_config_clone(self).await?;
        let secrets = self.current_secrets();
        let kind =
            ModuleKind::parse(module).map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
        let list = offload_boundary(move || {
            let mut store = ModuleStore::open(&cfg.data_dir)?;
            store.set(&cfg.browser, kind, enabled)?;
            statuses(&cfg.data_dir, &cfg.browser)
        })
        .await
        .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
        Ok(list
            .into_iter()
            .map(|status| map_module_status(status, &secrets))
            .collect())
    }

    pub async fn list_mcp_servers(&self) -> Result<Vec<McpServerDto>, DesktopError> {
        let cfg = require_config_clone(self).await?;
        let secrets = self.current_secrets();
        Ok(build_mcp_list(&cfg, &secrets))
    }

    pub async fn toggle_mcp_server(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<Vec<McpServerDto>, DesktopError> {
        let _ = require_idle_config_clone(self).await?;
        let secrets = self.current_secrets();
        let mut guard = self.inner.lock().await;
        let cfg = guard
            .config
            .as_mut()
            .ok_or_else(|| DesktopError::new("invalid_config", "Configuration is not loaded"))?;
        let server = cfg.mcp.servers.get_mut(name).ok_or_else(|| {
            DesktopError::new("mcp_not_found", format!("MCP server '{name}' not found"))
        })?;
        server.enabled = enabled;
        Ok(build_mcp_list(cfg, &secrets))
    }

    pub async fn list_credentials(&self) -> Result<Vec<CredentialRecordDto>, DesktopError> {
        let cfg = require_config_clone(self).await?;
        let secrets = self.current_secrets();
        let store = open_credential_store(&cfg.state_dir, self.secrets.clone(), &secrets).await?;
        let list = store
            .list()
            .await
            .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
        Ok(list
            .into_iter()
            .map(|record| map_credential_metadata(record, &secrets))
            .collect())
    }

    pub async fn import_credential(
        &self,
        server: &str,
        env_var: &str,
        scopes: Vec<String>,
        approve_scopes: bool,
    ) -> Result<CredentialRecordDto, DesktopError> {
        if !approve_scopes {
            return Err(DesktopError::new(
                "approval_required",
                "Explicit scope approval is required before importing a credential",
            ));
        }
        let cfg = require_config_clone(self).await?;
        let secrets = self.current_secrets();
        let server_cfg = cfg.mcp.servers.get(server).ok_or_else(|| {
            DesktopError::new("mcp_not_found", format!("MCP server '{server}' not found"))
        })?;
        let McpTransportSettings::Http(http) = &server_cfg.transport else {
            return Err(DesktopError::new(
                "invalid_transport",
                "Credentials can only be imported for HTTP MCP servers",
            ));
        };
        let secret_str = cfg
            .secret(env_var)
            .map(str::to_owned)
            .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
        let secret_val = SecretValue::new(secret_str.clone())
            .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
        let _ = self.secrets.register(&[secret_str]);

        let scope_set: BTreeSet<String> = if scopes.is_empty() {
            http.scopes.clone()
        } else {
            scopes
                .into_iter()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect()
        };
        let scope = CredentialScope::for_mcp(server, &http.endpoint, scope_set)
            .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;

        let all_secrets = self.current_secrets();
        let scope_json = serde_json::json!(&scope).to_string();
        if soyo_core::redaction::contains_secret(&scope_json, &all_secrets) {
            return Err(DesktopError::new(
                "sensitive_data",
                "MCP credential scope contains sensitive value",
            ));
        }

        let store =
            open_credential_store(&cfg.state_dir, self.secrets.clone(), &all_secrets).await?;
        let reference = store
            .create(scope, CredentialValue::Secret(secret_val))
            .await
            .map_err(|err| DesktopError::from_boundary(&err, &all_secrets))?;

        let list = store
            .list()
            .await
            .map_err(|err| DesktopError::from_boundary(&err, &all_secrets))?;
        let record = list
            .into_iter()
            .find(|r| r.reference == reference)
            .ok_or_else(|| DesktopError::new("storage_error", "Created credential missing"))?;
        Ok(map_credential_metadata(record, &all_secrets))
    }

    pub async fn revoke_credential(&self, reference: &str) -> Result<bool, DesktopError> {
        let cfg = require_config_clone(self).await?;
        let secrets = self.current_secrets();
        let cred_ref: CredentialRef = reference
            .parse()
            .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
        let store = open_credential_store(&cfg.state_dir, self.secrets.clone(), &secrets).await?;
        let list = store
            .list()
            .await
            .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
        let meta = list
            .into_iter()
            .find(|r| r.reference == cred_ref)
            .ok_or_else(|| {
                DesktopError::new("invalid_credential_ref", "Credential reference not found")
            })?;
        store
            .revoke(cred_ref, &meta.scope)
            .await
            .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
        Ok(true)
    }

    pub async fn query_memory(&self, command: &str) -> Result<MemoryQueryResultDto, DesktopError> {
        let cfg = require_config_clone(self).await?;
        let secrets = self.current_secrets();
        let registry = self.secrets.clone();
        let cmd = command.to_owned();
        let (settings, data) = offload_boundary(move || {
            let admin = MemoryAdmin::open(&cfg, registry)?;
            let settings = admin.settings()?;
            let cancel = AtomicBool::new(false);
            let mut full = admin.query(&cmd, &cancel)?;
            let inner = full
                .as_object_mut()
                .and_then(|obj| obj.remove("data"))
                .unwrap_or(full);
            Ok((settings, inner))
        })
        .await
        .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;

        Ok(MemoryQueryResultDto { settings, data })
    }

    pub async fn prepare_memory_action(
        &self,
        command: &str,
    ) -> Result<PreparedMemoryReviewDto, DesktopError> {
        let cfg = require_idle_config_clone(self).await?;
        let secrets = self.current_secrets();
        let registry = self.secrets.clone();
        let cmd = command.to_owned();
        let prepared = offload_boundary(move || {
            let admin = MemoryAdmin::open(&cfg, registry)?;
            let cancel = AtomicBool::new(false);
            admin.prepare(&cmd, &cancel)
        })
        .await
        .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;

        let action_id = format!("mem_{}", Uuid::now_v7().simple());
        let dto = PreparedMemoryReviewDto {
            action_id: action_id.clone(),
            operation_id: prepared.operation_id().to_owned(),
            expected_revision: prepared.expected_revision().to_owned(),
            review: prepared.review().clone(),
        };

        let mut actions = self
            .pending_memory_actions
            .lock()
            .map_err(|_| DesktopError::new("state_lock", "memory action store lock failed"))?;
        if actions.len() >= 16 {
            actions.clear();
        }
        actions.insert(action_id, prepared);
        Ok(dto)
    }

    pub async fn confirm_memory_action(
        &self,
        action_id: &str,
    ) -> Result<MemoryConfirmResultDto, DesktopError> {
        let cfg = require_idle_config_clone(self).await?;
        let secrets = self.current_secrets();
        let prepared = {
            let mut actions = self
                .pending_memory_actions
                .lock()
                .map_err(|_| DesktopError::new("state_lock", "memory action store lock failed"))?;
            actions.remove(action_id).ok_or_else(|| {
                DesktopError::new(
                    "memory_action_not_found",
                    "Prepared memory action expired or was already confirmed",
                )
            })?
        };
        let registry = self.secrets.clone();
        let result = offload_boundary(move || {
            let admin = MemoryAdmin::open(&cfg, registry)?;
            let cancel = AtomicBool::new(false);
            admin.confirm(prepared, &cancel)
        })
        .await
        .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
        Ok(MemoryConfirmResultDto { result })
    }

    pub async fn export_session(
        &self,
        session_id: Option<String>,
        target_path: &str,
        format: ExportFormatDto,
        scope: ExportScopeDto,
        include_tools: bool,
    ) -> Result<ExportResultDto, DesktopError> {
        self.export_session_for(0, session_id, target_path, format, scope, include_tools)
            .await
    }

    pub async fn export_session_for(
        &self,
        conn_id: u64,
        session_id: Option<String>,
        target_path: &str,
        format: ExportFormatDto,
        scope: ExportScopeDto,
        include_tools: bool,
    ) -> Result<ExportResultDto, DesktopError> {
        let cfg = require_config_clone(self).await?;
        let secrets = self.current_secrets();
        let trimmed_target = target_path.trim();
        if trimmed_target.is_empty() {
            return Err(DesktopError::new(
                "invalid_args",
                "Export target file path cannot be empty",
            ));
        }

        let resolved_id: SessionId = if let Some(raw_id) = session_id.as_deref() {
            session::parse_id(raw_id).map_err(|err| {
                DesktopError::new(
                    "invalid_session_id",
                    redaction::redact_str(&err.to_string(), &secrets),
                )
            })?
        } else {
            let guard = self.inner.lock().await;
            guard.resolve_session_id(None, conn_id).ok_or_else(|| {
                DesktopError::new("session_required", "Select or create a session to export")
            })?
        };

        let cancel = Arc::new(RunCancellation::new());
        let snapshot = SqliteStore::read_snapshot(
            &cfg.data_dir,
            resolved_id,
            SnapshotLimits::default(),
            cancel.clone(),
        )
        .await
        .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;

        let workspace = cfg.cwd.clone();
        let mode = cfg.mode;
        let requested = PathBuf::from(trimmed_target);
        let target = if requested.is_absolute() {
            requested
        } else {
            workspace.join(requested)
        };
        let export_options = ExportOptions {
            scope: match scope {
                ExportScopeDto::Transcript => ExportScope::Transcript,
                ExportScopeDto::Full => ExportScope::Full,
            },
            include_tools,
            ..ExportOptions::default()
        };
        let secrets_clone = secrets.clone();
        let target_clone = target.clone();
        let (bytes_len, receipt) = offload_boundary(move || {
            let export = SessionExport::from_snapshot(&snapshot, export_options, &secrets_clone)?;
            let bytes = match format {
                ExportFormatDto::Json => export.to_json()?,
                ExportFormatDto::Markdown => export.to_markdown()?,
            };
            let len = bytes.len() as u64;
            let mut receipt = soyo_app::export::publish_session(
                &target_clone,
                &bytes,
                &workspace,
                &snapshot.session.workspace,
                mode,
                cancel.as_ref(),
            )?;
            receipt["session_id"] = serde_json::json!(resolved_id);
            receipt["format"] = match format {
                ExportFormatDto::Json => "json",
                ExportFormatDto::Markdown => "markdown",
            }
            .into();
            soyo_core::redaction::redact(&mut receipt, &secrets_clone)?;
            Ok((len, receipt))
        })
        .await
        .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;

        let _ = error_codes::STORAGE_ERROR;
        Ok(ExportResultDto {
            path: redaction::redact_str(&target.to_string_lossy(), &secrets),
            format,
            scope,
            bytes: bytes_len,
            receipt,
        })
    }

    pub async fn stage_attachments(
        &self,
        paths: Vec<String>,
    ) -> Result<Vec<StagedAttachmentDto>, DesktopError> {
        let secrets = self.current_secrets();
        let drafts = self.staged_drafts.clone();
        let secrets_for_worker = secrets.clone();
        offload_boundary(move || {
            attachment::stage_attachment_files(&paths, &drafts, &secrets_for_worker)
                .map_err(|err| BoundaryError::new(err.code, err.message))
        })
        .await
        .map_err(|err| DesktopError::from_boundary(&err, &secrets))
    }
}

struct MergeSetup {
    name: String,
    protocol: String,
    base_url: String,
    model: String,
    auth: String,
    key_env: Option<String>,
    context_window: u64,
    max_output: u32,
    permissions: Option<&'static str>,
    mode: Option<&'static str>,
}

fn permission_label(mode: PermissionModeDto) -> &'static str {
    match mode {
        PermissionModeDto::Auto => "auto",
        PermissionModeDto::Bypass => "bypass",
        PermissionModeDto::Manual => "manual",
    }
}

fn work_mode_label(mode: WorkModeDto) -> &'static str {
    match mode {
        WorkModeDto::Plan => "plan",
        WorkModeDto::Build => "build",
        WorkModeDto::Code => "code",
        WorkModeDto::Work => "work",
    }
}

/// Merge the setup profile into `config_path`, preserving an existing valid
/// document (comments and unrelated sections) via `toml_edit`. Returns the
/// config path and an optional backup of the previous file so a failed
/// reload can restore it.
fn merge_setup_config(
    config_path: &Path,
    setup: &MergeSetup,
) -> Result<(PathBuf, Option<PathBuf>), BoundaryError> {
    let path = config_path.to_path_buf();
    let text = fs::read_to_string(&path).unwrap_or_default();
    let had_file = !text.trim().is_empty();
    let backup = if had_file {
        let backup_path = path.with_file_name("config.toml.bak");
        fs::copy(&path, &backup_path).map_err(|_| {
            BoundaryError::new("config_io", "Cannot back up existing configuration")
        })?;
        Some(backup_path)
    } else {
        None
    };

    // Fresh start for a missing or unparseable file: schema_version must sit
    // before any table header, so seed the document text first.
    let base = match text.parse::<toml_edit::DocumentMut>() {
        Ok(_) if had_file => text.clone(),
        _ => "schema_version = 1\n".to_owned(),
    };
    let mut doc = base
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| BoundaryError::new("config_io", "Cannot rebuild configuration document"))?;
    if doc.get("schema_version").is_none() {
        doc = format!("schema_version = 1\n{doc}")
            .parse::<toml_edit::DocumentMut>()
            .map_err(|_| {
                BoundaryError::new("config_io", "Cannot place schema_version in configuration")
            })?;
    }

    if !doc.get("defaults").is_some_and(|item| item.is_table()) {
        doc["defaults"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let Some(defaults) = doc["defaults"].as_table_mut() else {
        return Err(BoundaryError::new("config_io", "Invalid defaults section"));
    };
    defaults["profile"] = toml_edit::value(setup.name.as_str());
    if let Some(permissions) = setup.permissions {
        defaults["permissions"] = toml_edit::value(permissions);
    }
    if let Some(mode) = setup.mode {
        defaults["mode"] = toml_edit::value(mode);
    }
    if defaults.get("mode").is_none() {
        defaults["mode"] = toml_edit::value("work");
    }
    if defaults.get("permissions").is_none() {
        defaults["permissions"] = toml_edit::value("auto");
    }
    if defaults.get("output").is_none() {
        defaults["output"] = toml_edit::value("text");
    }

    if !doc.get("profiles").is_some_and(|item| item.is_table()) {
        doc["profiles"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let Some(profiles) = doc["profiles"].as_table_mut() else {
        return Err(BoundaryError::new("config_io", "Invalid profiles section"));
    };
    let mut profile = toml_edit::Table::new();
    profile["protocol"] = toml_edit::value(setup.protocol.as_str());
    profile["base_url"] = toml_edit::value(setup.base_url.as_str());
    profile["model"] = toml_edit::value(setup.model.as_str());
    profile["auth"] = toml_edit::value(setup.auth.as_str());
    if let Some(env_name) = &setup.key_env {
        profile["api_key_env"] = toml_edit::value(env_name.as_str());
    }
    profile["context_window_tokens"] = toml_edit::value(setup.context_window as i64);
    profile["max_output_tokens"] = toml_edit::value(setup.max_output as i64);
    profiles[setup.name.as_str()] = toml_edit::Item::Table(profile);

    if let Some(parent) = path.parent() {
        soyo_fs::ensure_root_dir(parent, "config_io", "configuration")?;
    }
    fs::write(&path, doc.to_string())
        .map_err(|_| BoundaryError::new("config_io", "Cannot write configuration file"))?;
    Ok((path, backup))
}

/// Best-effort restore after a failed reload: put the backup back, or remove
/// the file we just created when there was none before.
fn restore_config_backup(config_path: &Path, backup_path: Option<PathBuf>) {
    match backup_path {
        Some(backup) => {
            let _ = fs::copy(&backup, config_path);
        }
        None => {
            let _ = fs::remove_file(config_path);
        }
    }
}
