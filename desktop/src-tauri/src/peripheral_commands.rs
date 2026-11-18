//! Peripheral Tauri commands and runtime helpers (settings, extensions, modules, MCP/credentials, memory, export, attachments).

use crate::{
    attachment_protocol, redaction,
    runtime_bridge::DesktopState,
    types::{
        BackgroundJobDto, BootstrapStateDto, CredentialRecordDto, DesktopError, ExportFormatDto,
        ExportResultDto, ExportScopeDto, ExtensionItemDto, McpServerDto, MemoryConfirmResultDto,
        MemoryQueryResultDto, ModuleStatusDto, PreparedMemoryReviewDto, SandboxBackendDto,
        StagedAttachmentDto,
    },
};
use soyo_app::{
    interaction::RunCancellation, memory_admin::MemoryAdmin, offload_boundary, session,
};
use soyo_core::{
    background::JobStatus,
    config::{Config, ThinkingEffort},
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
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};
use tauri::State;
use uuid::Uuid;

impl From<SandboxBackendDto> for BackendKind {
    fn from(value: SandboxBackendDto) -> Self {
        match value {
            SandboxBackendDto::Auto => Self::Auto,
            SandboxBackendDto::Seatbelt => Self::Seatbelt,
            SandboxBackendDto::Bubblewrap => Self::Bubblewrap,
            SandboxBackendDto::Host => Self::Host,
        }
    }
}

async fn require_config_clone(state: &DesktopState) -> Result<Config, DesktopError> {
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

async fn require_idle_config_clone(state: &DesktopState) -> Result<Config, DesktopError> {
    {
        let control = state
            .active_control
            .lock()
            .map_err(|_| DesktopError::new("state_lock", "active control lock failed"))?;
        if let Some(active) = control.as_ref() {
            if active.pending.is_some() {
                return Err(DesktopError::new(
                    "pending_request",
                    "Resolve pending approval or input first",
                ));
            }
            return Err(DesktopError::new(
                "run_active",
                "Cancel or wait for the active run first",
            ));
        }
    }
    let guard = state.inner.lock().await;
    if guard.active_run_id.is_some() {
        return Err(DesktopError::new(
            "run_active",
            "Cancel or wait for the active run first",
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

impl DesktopState {
    pub async fn set_workspace(&self, path: &str) -> Result<BootstrapStateDto, DesktopError> {
        let cfg = require_idle_config_clone(self).await?;
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
            if guard.active_run_id.is_some() {
                return Err(DesktopError::new(
                    "run_active",
                    "Cancel or wait for the active run before changing workspace",
                ));
            }
            let profile_lookup = guard
                .config
                .as_ref()
                .and_then(|c| {
                    guard
                        .prepared
                        .as_ref()
                        .and_then(|p| c.profiles.get(&p.session.profile))
                })
                .cloned();
            if let Some(prepared) = &mut guard.prepared {
                let Some(profile) = profile_lookup else {
                    return Err(DesktopError::new(
                        "invalid_config",
                        "Session profile not found in configuration",
                    ));
                };
                let fingerprint = profile
                    .fingerprint()
                    .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
                let mut updated = prepared.session.clone();
                updated.workspace = workspace.clone();
                prepared
                    .store
                    .update_session_config(&updated, &fingerprint)
                    .await
                    .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
                prepared.session = updated;
            }
            if let Some(cfg_mut) = &mut guard.config {
                cfg_mut.cwd = workspace;
            }
        }

        Ok(self.get_bootstrap_state().await)
    }

    pub async fn select_profile(&self, profile: &str) -> Result<BootstrapStateDto, DesktopError> {
        let _ = require_idle_config_clone(self).await?;
        let mut guard = self.inner.lock().await;
        if guard.active_run_id.is_some() {
            return Err(DesktopError::new(
                "run_active",
                "Cancel or wait for the active run before changing profile",
            ));
        }
        if let Some(prepared) = &guard.prepared
            && prepared.session.profile != profile
        {
            return Err(DesktopError::new(
                "session_profile_locked",
                "Start a new session before switching to a different model profile",
            ));
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
        Ok(self.get_bootstrap_state().await)
    }

    pub async fn set_thinking_effort(
        &self,
        effort: Option<String>,
    ) -> Result<BootstrapStateDto, DesktopError> {
        let _ = require_idle_config_clone(self).await?;
        let parsed = parse_effort(effort.as_deref())?;
        let mut guard = self.inner.lock().await;
        if guard.active_run_id.is_some() {
            return Err(DesktopError::new(
                "run_active",
                "Cancel or wait for the active run before changing thinking effort",
            ));
        }
        let cfg = guard
            .config
            .as_mut()
            .ok_or_else(|| DesktopError::new("invalid_config", "Configuration is not loaded"))?;
        let Some(profile_name) = cfg.profile.clone() else {
            return Err(DesktopError::new(
                "invalid_config",
                "Select a configured profile first",
            ));
        };
        let profile = cfg
            .profiles
            .get_mut(&profile_name)
            .ok_or_else(|| DesktopError::new("invalid_config", "Selected profile is missing"))?;
        profile.thinking_effort = parsed;
        drop(guard);
        Ok(self.get_bootstrap_state().await)
    }

    pub async fn set_sandbox_backend(
        &self,
        backend: SandboxBackendDto,
    ) -> Result<BootstrapStateDto, DesktopError> {
        let _ = require_idle_config_clone(self).await?;
        let mut guard = self.inner.lock().await;
        if guard.active_run_id.is_some() {
            return Err(DesktopError::new(
                "run_active",
                "Cancel or wait for the active run before changing sandbox backend",
            ));
        }
        let cfg = guard
            .config
            .as_mut()
            .ok_or_else(|| DesktopError::new("invalid_config", "Configuration is not loaded"))?;
        cfg.sandbox.backend = backend.into();
        drop(guard);
        Ok(self.get_bootstrap_state().await)
    }

    pub async fn set_autocompact(
        &self,
        enabled: bool,
        trigger_tokens: Option<u64>,
    ) -> Result<BootstrapStateDto, DesktopError> {
        let _ = require_idle_config_clone(self).await?;
        let secrets = self.current_secrets();
        let mut guard = self.inner.lock().await;
        if guard.active_run_id.is_some() {
            return Err(DesktopError::new(
                "run_active",
                "Cancel or wait for the active run before changing compaction settings",
            ));
        }
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
        let secrets = self.current_secrets();
        let (store, session_id) = {
            let guard = self.inner.lock().await;
            let Some(prepared) = &guard.prepared else {
                return Ok(Vec::new());
            };
            (prepared.store.clone(), prepared.session.id)
        };
        let jobs = store
            .background_jobs(session_id)
            .await
            .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
        let active_handle = self
            .active_control
            .lock()
            .ok()
            .and_then(|g| g.as_ref().and_then(|c| c.handle.clone()));

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

    pub fn cancel_background_job(&self, job_id: &str) -> Result<bool, DesktopError> {
        let secrets = self.current_secrets();
        let control_guard = self
            .active_control
            .lock()
            .map_err(|_| DesktopError::new("state_lock", "active control lock failed"))?;
        let Some(control) = control_guard.as_ref() else {
            return Err(DesktopError::new(
                "no_active_run",
                "Background job cancellation requires an active run",
            ));
        };
        let handle = control
            .handle
            .as_ref()
            .ok_or_else(|| DesktopError::new("run_starting", "Run is still initializing"))?;
        handle
            .cancel_background(job_id)
            .map_err(|err| DesktopError::from_boundary(&err, &secrets))
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
        if guard.active_run_id.is_some() {
            return Err(DesktopError::new(
                "run_active",
                "Cancel or wait for the active run before toggling MCP servers",
            ));
        }
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
            guard
                .prepared
                .as_ref()
                .map(|p| p.session.id)
                .ok_or_else(|| {
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
            attachment_protocol::stage_attachment_files(&paths, &drafts, &secrets_for_worker)
                .map_err(|err| BoundaryError::new(err.code, err.message))
        })
        .await
        .map_err(|err| DesktopError::from_boundary(&err, &secrets))
    }
}

#[tauri::command]
#[specta::specta]
pub async fn set_workspace(
    state: State<'_, DesktopState>,
    path: String,
) -> Result<BootstrapStateDto, DesktopError> {
    state.set_workspace(&path).await
}

#[tauri::command]
#[specta::specta]
pub async fn select_profile(
    state: State<'_, DesktopState>,
    profile: String,
) -> Result<BootstrapStateDto, DesktopError> {
    state.select_profile(&profile).await
}

#[tauri::command]
#[specta::specta]
pub async fn set_thinking_effort(
    state: State<'_, DesktopState>,
    effort: Option<String>,
) -> Result<BootstrapStateDto, DesktopError> {
    state.set_thinking_effort(effort).await
}

#[tauri::command]
#[specta::specta]
pub async fn set_sandbox_backend(
    state: State<'_, DesktopState>,
    backend: SandboxBackendDto,
) -> Result<BootstrapStateDto, DesktopError> {
    state.set_sandbox_backend(backend).await
}

#[tauri::command]
#[specta::specta]
pub async fn set_autocompact(
    state: State<'_, DesktopState>,
    enabled: bool,
    trigger_tokens: Option<f64>,
) -> Result<BootstrapStateDto, DesktopError> {
    let parsed_tokens = match trigger_tokens {
        Some(value) => {
            if !value.is_finite() || value <= 0.0 || value > 9_007_199_254_740_991.0 {
                return Err(DesktopError::new(
                    "invalid_args",
                    "Auto-compact trigger tokens must be a valid positive integer",
                ));
            }
            Some(value.trunc() as u64)
        }
        None => None,
    };
    state.set_autocompact(enabled, parsed_tokens).await
}

#[tauri::command]
#[specta::specta]
pub async fn list_extensions(
    state: State<'_, DesktopState>,
) -> Result<Vec<ExtensionItemDto>, DesktopError> {
    state.list_extensions().await
}

#[tauri::command]
#[specta::specta]
pub async fn list_jobs(
    state: State<'_, DesktopState>,
) -> Result<Vec<BackgroundJobDto>, DesktopError> {
    state.list_jobs().await
}

#[tauri::command]
#[specta::specta]
pub async fn cancel_background_job(
    state: State<'_, DesktopState>,
    job_id: String,
) -> Result<bool, DesktopError> {
    state.cancel_background_job(&job_id)
}

#[tauri::command]
#[specta::specta]
pub async fn list_modules(
    state: State<'_, DesktopState>,
) -> Result<Vec<ModuleStatusDto>, DesktopError> {
    state.list_modules().await
}

#[tauri::command]
#[specta::specta]
pub async fn set_module_enabled(
    state: State<'_, DesktopState>,
    module: String,
    enabled: bool,
) -> Result<Vec<ModuleStatusDto>, DesktopError> {
    state.set_module_enabled(&module, enabled).await
}

#[tauri::command]
#[specta::specta]
pub async fn list_mcp_servers(
    state: State<'_, DesktopState>,
) -> Result<Vec<McpServerDto>, DesktopError> {
    state.list_mcp_servers().await
}

#[tauri::command]
#[specta::specta]
pub async fn toggle_mcp_server(
    state: State<'_, DesktopState>,
    name: String,
    enabled: bool,
) -> Result<Vec<McpServerDto>, DesktopError> {
    state.toggle_mcp_server(&name, enabled).await
}

#[tauri::command]
#[specta::specta]
pub async fn list_credentials(
    state: State<'_, DesktopState>,
) -> Result<Vec<CredentialRecordDto>, DesktopError> {
    state.list_credentials().await
}

#[tauri::command]
#[specta::specta]
pub async fn import_credential(
    state: State<'_, DesktopState>,
    server: String,
    env_var: String,
    scopes: Vec<String>,
    approve_scopes: bool,
) -> Result<CredentialRecordDto, DesktopError> {
    state
        .import_credential(&server, &env_var, scopes, approve_scopes)
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn revoke_credential(
    state: State<'_, DesktopState>,
    reference: String,
) -> Result<bool, DesktopError> {
    state.revoke_credential(&reference).await
}

#[tauri::command]
#[specta::specta]
pub async fn query_memory(
    state: State<'_, DesktopState>,
    command: String,
) -> Result<MemoryQueryResultDto, DesktopError> {
    state.query_memory(&command).await
}

#[tauri::command]
#[specta::specta]
pub async fn prepare_memory_action(
    state: State<'_, DesktopState>,
    command: String,
) -> Result<PreparedMemoryReviewDto, DesktopError> {
    state.prepare_memory_action(&command).await
}

#[tauri::command]
#[specta::specta]
pub async fn confirm_memory_action(
    state: State<'_, DesktopState>,
    action_id: String,
) -> Result<MemoryConfirmResultDto, DesktopError> {
    state.confirm_memory_action(&action_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn export_session(
    state: State<'_, DesktopState>,
    session_id: Option<String>,
    target_path: String,
    format: ExportFormatDto,
    scope: ExportScopeDto,
    include_tools: bool,
) -> Result<ExportResultDto, DesktopError> {
    state
        .export_session(session_id, &target_path, format, scope, include_tools)
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn stage_attachments(
    state: State<'_, DesktopState>,
    paths: Vec<String>,
) -> Result<Vec<StagedAttachmentDto>, DesktopError> {
    state.stage_attachments(paths).await
}
