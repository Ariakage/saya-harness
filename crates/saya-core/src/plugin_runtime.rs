//! Host-controlled Run assembly for installed plugins.
//!
//! This component acquires immutable leases and builds service definitions. The
//! host owns current permission checks, native backend preparation, real process
//! startup, and awaited shutdown. A descriptor's ceiling must accompany its MCP
//! settings into dispatch. Synchronous filesystem work belongs on a worker.
use crate::{
    config::Environment,
    contract::{BoundaryError, ErrorInfo, RunId, SessionId},
    credentials::{CredentialRef, SecretRegistry},
    mcp::config::{
        HttpMcpSettings, McpLimitSettings, McpServerSettings, McpSettings, McpTransportSettings,
        StdioMcpSettings,
    },
    plugin_manifest::{HookEntry, McpEntry},
    plugin_store::{
        CleanupOutcome, EnableGrant, LeaseRecord, PluginLease, PluginScope, PluginStore,
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

mod assembly;
#[cfg(test)]
mod tests;
mod view;
pub use view::{PluginView, ViewIdentity, ViewInventory, ViewLimits, ViewPool};

fn error(code: &'static str, message: &'static str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}
fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn cancelled(cancel: &AtomicBool) -> Result<(), BoundaryError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(error("cancelled", "Plugin Run preparation was cancelled"));
    }
    Ok(())
}
fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn validate_lease_workspace(lease: &PluginLease) -> Result<(), BoundaryError> {
    let PluginScope::Workspace { path, .. } = &lease.record().scope else {
        return Err(error(
            "plugin_scope",
            "Plugin Run lease lacks a workspace identity",
        ));
    };
    if PluginScope::workspace(path)? != lease.record().scope {
        return Err(error(
            "plugin_scope",
            "Plugin Run workspace identity changed",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginEntryIdentity {
    pub plugin_id: String,
    pub package_digest: String,
    pub entry_id: String,
}
impl PluginEntryIdentity {
    pub fn key(&self) -> String {
        format!(
            "{}@{}/{}",
            self.plugin_id, self.package_digest, self.entry_id
        )
    }
    pub fn server_id(&self) -> String {
        format!("sp_{}", &hash(self.key().as_bytes())[..29])
    }
    pub fn model_alias(&self, remote_name: &str) -> Result<String, BoundaryError> {
        crate::registry::mcp_model_name(&self.server_id(), remote_name)
    }
}
/// Key a host binding to one exact package version, not a publisher's display name.
pub fn slot_key(plugin_id: &str, digest: &str, slot: &str) -> String {
    format!("{plugin_id}@{digest}/{slot}")
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApprovedRuntime {
    pub program: PathBuf,
    pub args_prefix: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub read_roots: Vec<PathBuf>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CredentialSlotBinding {
    Environment {
        destination: String,
        reference: String,
    },
    HttpBearer {
        reference: String,
        audience: String,
    },
    HttpHeader {
        name: String,
        reference: String,
        audience: String,
    },
    HttpCredential {
        credential_ref: CredentialRef,
        audience: String,
        #[serde(default)]
        scopes: BTreeSet<String>,
    },
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HostEntrySettings {
    pub read_only_tools: BTreeSet<String>,
    pub limits: McpLimitSettings,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PluginHostBindings {
    pub runtimes: BTreeMap<String, ApprovedRuntime>,
    pub slots: BTreeMap<String, CredentialSlotBinding>,
    /// Keys are `plugin_id@full_digest/entry_id` and never implicit wildcards.
    pub entries: BTreeMap<String, HostEntrySettings>,
}

pub struct PluginRunContext {
    pub workspace: PathBuf,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub host_version: String,
    pub features: BTreeSet<String>,
    pub native_isolation_available: bool,
    pub protected_roots: Vec<PathBuf>,
    pub environment: Environment,
    pub secrets: Arc<SecretRegistry>,
    /// Actual host/base writable roots, including explicitly added roots. Views
    /// must be outside them so another Run tool cannot edit plugin code.
    pub host_write_roots: Vec<PathBuf>,
}
impl std::fmt::Debug for PluginRunContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PluginRunContext([HOST_BOUND])")
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceAccess {
    None,
    ReadOnly,
    WriteOnly,
    ReadWrite,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveCeiling {
    pub capabilities: BTreeSet<String>,
    pub network_origins: BTreeSet<String>,
    pub credential_slots: BTreeSet<String>,
    pub workspace: PathBuf,
    pub workspace_access: WorkspaceAccess,
    pub scope: PluginScope,
    pub enable_revision: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessPolicy {
    pub runtime_name: String,
    pub program: PathBuf,
    pub read_roots: Vec<PathBuf>,
    pub write_roots: Vec<PathBuf>,
    pub protected_roots: Vec<PathBuf>,
    pub package_view: ViewIdentity,
    pub environment_references: BTreeMap<String, String>,
    pub inherit_host_environment: bool,
    pub native_isolation_required: bool,
}
#[derive(Clone)]
pub struct PluginServerDescriptor {
    pub identity: PluginEntryIdentity,
    pub server_id: String,
    pub lease_id: String,
    pub capability_fingerprint: String,
    pub ceiling: EffectiveCeiling,
    pub settings: McpServerSettings,
    pub process: Option<ProcessPolicy>,
    environment: Environment,
}
impl std::fmt::Debug for PluginServerDescriptor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PluginServerDescriptor")
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}
impl PluginServerDescriptor {
    /// Contains only explicitly selected references and generated private paths.
    /// The host must use it instead of inheriting the full Run environment.
    pub fn environment(&self) -> &Environment {
        &self.environment
    }
    pub fn model_alias(&self, remote: &str) -> Result<String, BoundaryError> {
        self.identity.model_alias(remote)
    }
}
#[derive(Clone)]
pub struct PluginHookDescriptor {
    pub identity: PluginEntryIdentity,
    pub hook_id: String,
    pub lease_id: String,
    pub event: String,
    pub kind: String,
    pub timeout_ms: u32,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub ceiling: EffectiveCeiling,
    pub process: ProcessPolicy,
    environment: Environment,
}
impl std::fmt::Debug for PluginHookDescriptor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PluginHookDescriptor")
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}
impl PluginHookDescriptor {
    pub fn environment(&self) -> &Environment {
        &self.environment
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginDiagnostic {
    pub plugin_id: String,
    pub package_digest: String,
    pub entry_id: Option<String>,
    pub code: String,
    pub message: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevokedPlugin {
    pub lease: LeaseRecord,
    pub server_ids: Vec<String>,
    pub hook_ids: Vec<String>,
    pub reason: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginReconciliation {
    pub revoked: Vec<RevokedPlugin>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopEvidence {
    NeverStarted,
    Stopped,
    Unknown,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginCleanup {
    pub cleaned_leases: Vec<String>,
    pub cleanup_pending: Vec<LeaseRecord>,
    pub diagnostics: Vec<PluginDiagnostic>,
}

struct OwnedPlugin {
    lease: Option<Arc<PluginLease>>,
    view: Option<PluginView>,
    server_ids: Vec<String>,
    hook_ids: Vec<String>,
    revoked: bool,
    cleanup_uncertain: bool,
}
pub struct ActivePlugins {
    plugins: Vec<OwnedPlugin>,
    servers: BTreeMap<String, PluginServerDescriptor>,
    hooks: BTreeMap<String, PluginHookDescriptor>,
    diagnostics: Vec<PluginDiagnostic>,
}
impl std::fmt::Debug for ActivePlugins {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ActivePlugins")
            .field("servers", &self.servers.len())
            .field("hooks", &self.hooks.len())
            .finish_non_exhaustive()
    }
}
impl ActivePlugins {
    pub fn prepare(
        store: &PluginStore,
        context: &PluginRunContext,
        bindings: &PluginHostBindings,
        pool: Arc<ViewPool>,
        cancel: &AtomicBool,
    ) -> Result<Self, BoundaryError> {
        cancelled(cancel)?;
        assembly::validate_host(context, bindings, &pool)?;
        let scope = PluginScope::workspace(&context.workspace)?;
        let candidates: BTreeSet<_> = store
            .list()?
            .into_iter()
            .filter(|package| {
                package.enabled_scopes.iter().any(|enabled| {
                    enabled.enabled
                        && (enabled.scope == PluginScope::User || enabled.scope == scope)
                })
            })
            .map(|package| package.plugin_id)
            .collect();
        let mut active = Self {
            plugins: Vec::new(),
            servers: BTreeMap::new(),
            hooks: BTreeMap::new(),
            diagnostics: Vec::new(),
        };
        for plugin in candidates {
            if let Err(failure) = cancelled(cancel) {
                active.rollback_unstarted();
                return Err(failure);
            }
            let lease = match store.acquire(
                &plugin,
                &context.workspace,
                context.session_id,
                context.run_id,
                &context.host_version,
                &context.features,
            ) {
                Ok(lease) => Arc::new(lease),
                Err(failure) => {
                    active.diagnostics.push(PluginDiagnostic {
                        plugin_id: plugin,
                        package_digest: String::new(),
                        entry_id: None,
                        code: failure.0.code,
                        message: "Plugin scope or immutable content is unavailable".into(),
                    });
                    continue;
                }
            };
            let mut owned = OwnedPlugin {
                lease: Some(lease.clone()),
                view: None,
                server_ids: Vec::new(),
                hook_ids: Vec::new(),
                revoked: false,
                cleanup_uncertain: false,
            };
            let grant = lease.record().grant.clone();
            let mut assembler = assembly::Assembler {
                lease: &lease,
                context,
                bindings,
                pool: &pool,
                view: &mut owned.view,
                grant: &grant,
                cancel,
            };
            let result =
                active.assemble(&mut assembler, &mut owned.server_ids, &mut owned.hook_ids);
            if result
                .as_ref()
                .is_err_and(|failure| failure.0.code == "plugin_view_unknown")
            {
                owned.cleanup_uncertain = true;
            }
            active.plugins.push(owned);
            if let Err(failure) = result {
                drop(lease);
                active.rollback_unstarted();
                return Err(failure);
            }
        }
        if let Err(failure) = cancelled(cancel) {
            active.rollback_unstarted();
            return Err(failure);
        }
        Ok(active)
    }
    pub fn servers(&self) -> &BTreeMap<String, PluginServerDescriptor> {
        &self.servers
    }
    pub fn hooks(&self) -> &BTreeMap<String, PluginHookDescriptor> {
        &self.hooks
    }
    pub fn diagnostics(&self) -> &[PluginDiagnostic] {
        &self.diagnostics
    }
    pub fn skill_leases(&self) -> Vec<Arc<PluginLease>> {
        self.plugins
            .iter()
            .filter(|plugin| !plugin.revoked)
            .filter_map(|plugin| plugin.lease.clone())
            .collect()
    }

    /// Merge names for existing registration. The caller must retain each
    /// descriptor and enforce its ceiling when preparing/dispatching a server.
    pub fn settings_for_registration(
        &self,
        base: &McpSettings,
    ) -> Result<McpSettings, BoundaryError> {
        let mut settings = base.clone();
        for (id, server) in &self.servers {
            if settings
                .servers
                .insert(id.clone(), server.settings.clone())
                .is_some()
            {
                return Err(error(
                    "plugin_identity",
                    "Plugin server identity collides with a configured server",
                ));
            }
        }
        settings.validate()?;
        Ok(settings)
    }
    /// Call immediately before starting or dispatching an entry. This proves
    /// immutable package/view identity and current dependency enablement. The
    /// caller still enforces its current mode, policy, and native ceiling.
    pub fn check_entry(&self, id: &str) -> Result<(), BoundaryError> {
        let owned = self
            .plugins
            .iter()
            .find(|owned| {
                owned
                    .server_ids
                    .iter()
                    .chain(&owned.hook_ids)
                    .any(|candidate| candidate == id)
            })
            .ok_or_else(|| {
                error(
                    "plugin_entry_missing",
                    "Plugin entry is not held by this Run",
                )
            })?;
        if owned.revoked {
            return Err(error("plugin_revoked", "Plugin entry was revoked"));
        }
        let lease = owned
            .lease
            .as_ref()
            .ok_or_else(|| error("plugin_revoked", "Plugin entry no longer has a live lease"))?;
        validate_lease_workspace(lease)?;
        lease.check_active()?;
        lease.verify_contents()?;
        if let Some(view) = &owned.view {
            view.verify()?;
        }
        Ok(())
    }
    /// Recheck revocation without claiming that a real process has stopped.
    pub fn reconcile(&mut self) -> Result<PluginReconciliation, BoundaryError> {
        let mut revoked = Vec::new();
        for owned in &mut self.plugins {
            let Some(lease) = &owned.lease else {
                continue;
            };
            let validity = validate_lease_workspace(lease)
                .and_then(|()| lease.check_active())
                .and_then(|()| lease.verify_contents())
                .and_then(|()| match &owned.view {
                    Some(view) => view.verify(),
                    None => Ok(()),
                });
            if !owned.revoked
                && let Err(failure) = validity
            {
                owned.revoked = true;
                revoked.push(RevokedPlugin {
                    lease: lease.record().clone(),
                    server_ids: owned.server_ids.clone(),
                    hook_ids: owned.hook_ids.clone(),
                    reason: failure.0.code,
                });
            }
        }
        for item in &revoked {
            for id in &item.server_ids {
                self.servers.remove(id);
            }
            for id in &item.hook_ids {
                self.hooks.remove(id);
            }
        }
        Ok(PluginReconciliation { revoked })
    }

    /// Supply observed stop evidence for every registered MCP/Hook entry. Drop
    /// external Skill/catalog lease references first. Unknown retains pins/views.
    pub fn cleanup(
        &mut self,
        evidence: &BTreeMap<String, StopEvidence>,
        cancel: &AtomicBool,
    ) -> Result<PluginCleanup, BoundaryError> {
        let mut report = PluginCleanup {
            cleaned_leases: Vec::new(),
            cleanup_pending: Vec::new(),
            diagnostics: Vec::new(),
        };
        for owned in &mut self.plugins {
            cancelled(cancel)?;
            let Some(lease) = owned.lease.as_ref() else {
                continue;
            };
            let complete = owned.server_ids.iter().chain(&owned.hook_ids).all(|id| {
                matches!(
                    evidence.get(id),
                    Some(StopEvidence::Stopped | StopEvidence::NeverStarted)
                )
            });
            if !complete || owned.cleanup_uncertain || Arc::strong_count(lease) != 1 {
                report.cleanup_pending.push(lease.record().clone());
                continue;
            }
            if let Some(view) = &owned.view
                && let Err(failure) = view.remove(cancel)
            {
                report.diagnostics.push(diagnostic(
                    lease,
                    None,
                    failure.0.code,
                    "Plugin view cleanup is incomplete",
                ));
                report.cleanup_pending.push(lease.record().clone());
                continue;
            }
            owned.view = None;
            let lease = owned
                .lease
                .take()
                .ok_or_else(|| error("plugin_state", "Plugin lease ownership is unavailable"))?;
            let record = lease.record().clone();
            match Arc::try_unwrap(lease) {
                Ok(lease) => match lease.finish(CleanupOutcome::Cleaned) {
                    Ok(()) => report.cleaned_leases.push(record.lease_id.clone()),
                    Err(failure) => {
                        report.diagnostics.push(PluginDiagnostic {
                            plugin_id: record.plugin_id.clone(),
                            package_digest: record.digest.clone(),
                            entry_id: None,
                            code: failure.0.code,
                            message: "Plugin lease cleanup publication requires recovery".into(),
                        });
                        report.cleanup_pending.push(record);
                    }
                },
                Err(lease) => {
                    report.cleanup_pending.push(lease.record().clone());
                    owned.lease = Some(lease);
                }
            }
        }
        let cleaned: BTreeSet<_> = report.cleaned_leases.iter().cloned().collect();
        self.servers
            .retain(|_, server| !cleaned.contains(&server.lease_id));
        self.hooks
            .retain(|_, hook| !cleaned.contains(&hook.lease_id));
        Ok(report)
    }

    fn rollback_unstarted(&mut self) {
        // Nothing has escaped prepare, so no configured resource could have
        // started through this API. Failed cleanup stays conservatively pinned.
        let evidence = self
            .plugins
            .iter()
            .flat_map(|owned| owned.server_ids.iter().chain(&owned.hook_ids))
            .map(|id| (id.clone(), StopEvidence::NeverStarted))
            .collect();
        let _ = self.cleanup(&evidence, &AtomicBool::new(false));
    }
    fn assemble(
        &mut self,
        assembler: &mut assembly::Assembler<'_>,
        server_ids: &mut Vec<String>,
        hook_ids: &mut Vec<String>,
    ) -> Result<(), BoundaryError> {
        let lease = assembler.lease;
        let cancel = assembler.cancel;
        for entry in &lease.package().manifest().mcp {
            cancelled(cancel)?;
            if self.servers.len() >= 16 {
                self.diagnostics.push(diagnostic(
                    lease,
                    Some(entry.id()),
                    "plugin_limit".into(),
                    "Plugin MCP server count exceeds sixteen",
                ));
                continue;
            }
            let result = assembler.server(entry);
            match result {
                Ok(server)=>{let id=server.server_id.clone();if self.servers.contains_key(&id) {return Err(error("plugin_identity","Plugin server identity collides"));}server_ids.push(id.clone());self.servers.insert(id,server);},
                Err(failure) if matches!(failure.0.code.as_str(),"cancelled"|"plugin_view_unknown")=>return Err(failure),
                Err(failure)=>self.diagnostics.push(diagnostic(lease,Some(entry.id()),failure.0.code,"Plugin MCP entry cannot be configured under the current host bindings and ceiling")),
            }
        }
        for entry in &lease.package().manifest().hooks {
            cancelled(cancel)?;
            if self.hooks.len() >= 128 {
                self.diagnostics.push(diagnostic(
                    lease,
                    Some(&entry.id),
                    "plugin_limit".into(),
                    "Plugin Hook count exceeds its limit",
                ));
                continue;
            }
            match assembler.hook(entry) {
                Ok(hook)=>{let id=hook.hook_id.clone();if self.hooks.contains_key(&id) {return Err(error("plugin_identity","Plugin Hook identity collides"));}hook_ids.push(id.clone());self.hooks.insert(id,hook);},
                Err(failure) if matches!(failure.0.code.as_str(),"cancelled"|"plugin_view_unknown")=>return Err(failure),
                Err(failure)=>self.diagnostics.push(diagnostic(lease,Some(&entry.id),failure.0.code,"Plugin Hook entry cannot be configured under the current host bindings and ceiling")),
            }
        }
        Ok(())
    }
}
fn diagnostic(
    lease: &PluginLease,
    entry: Option<&str>,
    code: String,
    message: &'static str,
) -> PluginDiagnostic {
    PluginDiagnostic {
        plugin_id: lease.record().plugin_id.clone(),
        package_digest: lease.record().digest.clone(),
        entry_id: entry.map(str::to_owned),
        code,
        message: message.into(),
    }
}
