//! Private local plugin objects, explicit enablement, and process-owned leases.
//!
//! Methods perform synchronous, byte-bounded filesystem work and bounded lock
//! acquisition. Async hosts should call them on a blocking worker. Enablement
//! records a user-selected ceiling; current mode, deny rules, execution policy
//! and credentials remain the dispatcher's responsibility.
use crate::{
    contract::{BoundaryError, ErrorInfo, RunId, SessionId},
    credentials::SecretRegistry,
    plugin_manifest::{Manifest, PackageSource, PluginLock, VerifiedPackage},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};
use uuid::Uuid;

mod disk;
mod lease;
#[cfg(test)]
mod tests;
pub use lease::PluginLease;

const MAX_INDEX_BYTES: usize = 64 * 1024 * 1024;
const MAX_ENABLEMENTS: usize = 1024;

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
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StoreLimits {
    pub max_objects: usize,
    pub max_bytes: u64,
    pub max_leases: usize,
}
impl Default for StoreLimits {
    fn default() -> Self {
        Self {
            max_objects: 128,
            max_bytes: 512 * 1024 * 1024,
            max_leases: 1024,
        }
    }
}
impl StoreLimits {
    fn validate(self) -> Result<(), BoundaryError> {
        if self.max_objects == 0
            || self.max_objects > 1024
            || self.max_bytes == 0
            || self.max_leases == 0
            || self.max_leases > 8192
        {
            return Err(error("plugin_limit", "Plugin store limits are invalid"));
        }
        Ok(())
    }
}

/// Workspace identity prevents a scope grant from silently following a replaced
/// project directory at the same pathname. User scope is explicit and global.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginScope {
    User,
    Workspace {
        path: PathBuf,
        device: u64,
        inode: u64,
    },
}
impl PluginScope {
    pub fn workspace(path: &Path) -> Result<Self, BoundaryError> {
        disk::workspace_scope(path)
    }
    fn validate(&self) -> Result<(), BoundaryError> {
        if let Self::Workspace { path, inode, .. } = self
            && (!path.is_absolute()
                || path.to_str().is_none_or(|value| {
                    value.len() > 4096 || value.chars().any(crate::text::is_terminal_control)
                })
                || *inode == 0)
        {
            return Err(error(
                "plugin_scope",
                "Plugin workspace identity is invalid",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnableGrant {
    pub capabilities: BTreeSet<String>,
    pub network_origins: BTreeSet<String>,
    pub credential_slots: BTreeSet<String>,
}
impl EnableGrant {
    /// Prepare a ceiling for an explicit user enablement decision.
    pub fn requested(manifest: &Manifest) -> Self {
        Self {
            capabilities: manifest.capabilities.requested.iter().cloned().collect(),
            network_origins: manifest
                .capabilities
                .network_origins
                .iter()
                .cloned()
                .collect(),
            credential_slots: manifest
                .capabilities
                .credential_slots
                .iter()
                .cloned()
                .collect(),
        }
    }
    fn validate(&self, manifest: &Manifest) -> Result<(), BoundaryError> {
        let requested = Self::requested(manifest);
        if !self.capabilities.is_subset(&requested.capabilities)
            || !self.network_origins.is_subset(&requested.network_origins)
            || !self.credential_slots.is_subset(&requested.credential_slots)
        {
            return Err(error(
                "plugin_scope",
                "Enablement exceeds the package capability ceiling",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InstallationState {
    Staged,
    Installed,
    UninstallRequested,
    Corrupt,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InstanceState {
    Running,
    StopRequested,
    Unknown,
    Cleaned,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupOutcome {
    Cleaned,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnableRecord {
    pub plugin_id: String,
    pub digest: String,
    pub capability_fingerprint: String,
    pub lock_fingerprint: String,
    pub scope: PluginScope,
    pub enabled: bool,
    /// Explicit user stop also denies activation through a dependency closure.
    /// Closing future selection during update leaves existing pins usable.
    pub explicitly_disabled: bool,
    pub revision: u64,
    pub grant: EnableGrant,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeaseRecord {
    pub lease_id: String,
    pub plugin_id: String,
    pub digest: String,
    pub dependency_digests: Vec<String>,
    pub lock_fingerprint: String,
    pub capability_fingerprint: String,
    pub scope: PluginScope,
    pub enable_scope: PluginScope,
    pub enable_revision: u64,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub grant: EnableGrant,
    pub state: InstanceState,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSummary {
    pub plugin_id: String,
    pub version: String,
    pub digest: String,
    pub source: PackageSource,
    pub capability_fingerprint: String,
    /// Last validated declared ceiling. Enablement revalidates actual bytes.
    pub requested_capabilities: crate::plugin_manifest::Capabilities,
    pub state: InstallationState,
    pub selected: bool,
    pub source_conflict: bool,
    pub enabled_scopes: Vec<EnableRecord>,
    pub instances: Vec<LeaseRecord>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInspection {
    pub summary: PluginSummary,
    pub manifest: Manifest,
    pub lock: Option<PluginLock>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleChange {
    pub removed: bool,
    pub cleanup_pending: Vec<LeaseRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageRecord {
    manifest: Manifest,
    digest: String,
    capability_fingerprint: String,
    source: PackageSource,
    state: InstallationState,
    bytes: u64,
    lock: Option<PluginLock>,
    lock_fingerprint: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    plugin_id: String,
    digest: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    schema_version: u32,
    revision: u64,
    packages: Vec<PackageRecord>,
    selections: Vec<Selection>,
    enablements: Vec<EnableRecord>,
    leases: Vec<LeaseRecord>,
}
impl Index {
    fn empty() -> Self {
        Self {
            schema_version: 1,
            ..Self::default()
        }
    }
    fn next_revision(&self) -> Result<u64, BoundaryError> {
        self.revision
            .checked_add(1)
            .ok_or_else(|| error("plugin_limit", "Plugin store revision is exhausted"))
    }
    fn record(&self, digest: &str) -> Result<&PackageRecord, BoundaryError> {
        self.packages
            .iter()
            .find(|record| record.digest == digest)
            .ok_or_else(|| error("plugin_not_found", "Plugin content identity was not found"))
    }
    fn record_mut(&mut self, digest: &str) -> Result<&mut PackageRecord, BoundaryError> {
        self.packages
            .iter_mut()
            .find(|record| record.digest == digest)
            .ok_or_else(|| error("plugin_not_found", "Plugin content identity was not found"))
    }
}

struct Inner {
    disk: disk::Disk,
    secrets: Arc<SecretRegistry>,
    limits: StoreLimits,
}
#[derive(Clone)]
pub struct PluginStore {
    inner: Arc<Inner>,
}
impl std::fmt::Debug for PluginStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PluginStore([PRIVATE])")
    }
}
impl PluginStore {
    pub fn open(
        path: impl AsRef<Path>,
        secrets: Arc<SecretRegistry>,
        limits: StoreLimits,
    ) -> Result<Self, BoundaryError> {
        limits.validate()?;
        let store = Self {
            inner: Arc::new(Inner {
                disk: disk::Disk::open(path.as_ref())?,
                secrets,
                limits,
            }),
        };
        let _lock = store.inner.disk.lock_index()?;
        store.inner.read_index()?;
        Ok(store)
    }
    /// Include this entire private store in internal-root protection. A worker
    /// receives only an authenticated leased package subtree as read-only data.
    pub fn protected_path(&self) -> &Path {
        self.inner.disk.path()
    }

    /// Seal a candidate into private content-addressed storage. No source paths
    /// are reopened and the candidate is not enabled or executable afterward.
    pub fn stage(
        &self,
        package: &VerifiedPackage,
        source: PackageSource,
    ) -> Result<PluginSummary, BoundaryError> {
        source.validate()?;
        package.manifest().validate()?;
        self.inner.check_manifest_secrets(package)?;
        let _lock = self.inner.disk.lock_index()?;
        let mut index = self.inner.read_index()?;
        self.inner.disk.cleanup_staging()?;
        self.inner.disk.cleanup_unindexed(
            &index
                .packages
                .iter()
                .map(|record| record.digest.clone())
                .collect(),
        )?;
        if let Ok(existing) = index.record(package.digest()) {
            self.inner.load_verified(existing)?;
            return Ok(summary(&index, existing));
        }
        let bytes = package
            .files()
            .iter()
            .try_fold(0u64, |sum, file| sum.checked_add(file.bytes().len() as u64))
            .ok_or_else(|| error("plugin_limit", "Plugin byte count overflow"))?;
        let total = index
            .packages
            .iter()
            .try_fold(bytes, |sum, record| sum.checked_add(record.bytes));
        if index.packages.len() >= self.inner.limits.max_objects
            || total.is_none_or(|total| total > self.inner.limits.max_bytes)
        {
            return Err(error("plugin_limit", "Plugin store capacity is exhausted"));
        }
        self.inner.disk.stage_object(package)?;
        index.packages.push(PackageRecord {
            manifest: package.manifest().clone(),
            digest: package.digest().into(),
            capability_fingerprint: package.capability_fingerprint().into(),
            source,
            state: InstallationState::Staged,
            bytes,
            lock: None,
            lock_fingerprint: None,
        });
        self.inner.commit(&mut index)?;
        Ok(summary(&index, index.record(package.digest())?))
    }

    pub fn install(
        &self,
        package: &VerifiedPackage,
        source: PackageSource,
    ) -> Result<PluginSummary, BoundaryError> {
        self.stage(package, source)?;
        self.install_staged(package.digest())
    }

    pub fn install_staged(&self, digest: &str) -> Result<PluginSummary, BoundaryError> {
        let _lock = self.inner.disk.lock_index()?;
        let mut index = self.inner.read_index()?;
        let record = index.record(digest)?.clone();
        let package = self.inner.load_verified(&record)?;
        if record.state == InstallationState::Installed {
            return Ok(summary(&index, &record));
        }
        if record.state != InstallationState::Staged {
            return Err(error(
                "plugin_state",
                "Plugin candidate cannot be installed in this state",
            ));
        }
        let lock = build_lock(&index, &record)?;
        lock.verify_package(&package)?;
        self.inner.verify_lock(&index, &lock, Some(digest))?;
        let text = lock.to_toml()?;
        self.inner.disk.write_lock(digest, text.as_bytes())?;
        let installed = index.record_mut(digest)?;
        installed.lock_fingerprint = Some(digest_bytes(text.as_bytes()));
        installed.lock = Some(lock);
        installed.state = InstallationState::Installed;
        if !index
            .selections
            .iter()
            .any(|selection| selection.plugin_id == record.manifest.plugin.id)
        {
            index.selections.push(Selection {
                plugin_id: record.manifest.plugin.id.clone(),
                digest: digest.into(),
            });
        }
        self.inner.commit(&mut index)?;
        Ok(summary(&index, index.record(digest)?))
    }

    /// Store and select new content after an explicit update request. Existing
    /// leases keep their original identity. A new digest requires a new enable
    /// decision before future runs can acquire it.
    pub fn update(
        &self,
        expected_selected_digest: &str,
        package: &VerifiedPackage,
        source: PackageSource,
    ) -> Result<PluginSummary, BoundaryError> {
        self.install(package, source)?;
        let _lock = self.inner.disk.lock_index()?;
        let mut index = self.inner.read_index()?;
        let id = package.manifest().plugin.id.as_str();
        if !index.selections.iter().any(|selection| {
            selection.plugin_id == id && selection.digest == expected_selected_digest
        }) {
            return Err(error(
                "plugin_stale",
                "Selected plugin changed before update publication",
            ));
        }
        let previous = index.record(expected_selected_digest)?;
        if previous.manifest.plugin.id != id {
            return Err(error(
                "plugin_state",
                "Update targets a different plugin ID",
            ));
        }
        if package.digest() != expected_selected_digest {
            for record in &mut index.enablements {
                if record.plugin_id == id {
                    record.enabled = false;
                }
            }
            for selected in &mut index.selections {
                if selected.plugin_id == id {
                    selected.digest = package.digest().into();
                }
            }
            self.inner.commit(&mut index)?;
        }
        Ok(summary(&index, index.record(package.digest())?))
    }

    pub fn list(&self) -> Result<Vec<PluginSummary>, BoundaryError> {
        let _lock = self.inner.disk.lock_index()?;
        let mut index = self.inner.read_index()?;
        if self.inner.reconcile_leases(&mut index)? {
            self.inner.commit(&mut index)?;
        }
        Ok(index
            .packages
            .iter()
            .map(|record| summary(&index, record))
            .collect())
    }
    /// Read effective launch candidates without acquiring leases or reconciling
    /// historic instances. Workspace disables override user-level enablement.
    pub fn enabled_for(&self, workspace: &Path) -> Result<Vec<EnableRecord>, BoundaryError> {
        let scope = PluginScope::workspace(workspace)?;
        let _lock = self.inner.disk.lock_index()?;
        let index = self.inner.read_index()?;
        let ids: BTreeSet<_> = index
            .packages
            .iter()
            .map(|package| package.manifest.plugin.id.as_str())
            .collect();
        Ok(ids
            .into_iter()
            .filter_map(|id| {
                effective_enablement(&index, id, &scope)
                    .filter(|record| record.enabled)
                    .cloned()
            })
            .collect())
    }

    pub fn inspect(&self, digest: &str) -> Result<PluginInspection, BoundaryError> {
        let _lock = self.inner.disk.lock_index()?;
        let mut index = self.inner.read_index()?;
        let record = index.record(digest)?.clone();
        if let Err(failure) = self.inner.load_verified(&record) {
            index.record_mut(digest)?.state = InstallationState::Corrupt;
            let dependents: BTreeSet<_> = index
                .packages
                .iter()
                .filter(|package| {
                    package.digest == digest
                        || package.lock.as_ref().is_some_and(|lock| {
                            lock.packages.iter().any(|pin| pin.digest == digest)
                        })
                })
                .map(|package| package.digest.clone())
                .collect();
            for enabled in &mut index.enablements {
                if dependents.contains(&enabled.digest) {
                    enabled.enabled = false;
                }
            }
            for lease in &mut index.leases {
                if lease_uses(lease, digest) && lease.state == InstanceState::Running {
                    lease.state = InstanceState::StopRequested;
                }
            }
            self.inner.commit(&mut index)?;
            return Err(failure);
        }
        if self.inner.reconcile_leases(&mut index)? {
            self.inner.commit(&mut index)?;
        }
        Ok(PluginInspection {
            summary: summary(&index, index.record(digest)?),
            manifest: record.manifest,
            lock: record.lock,
        })
    }

    pub fn enable(
        &self,
        digest: &str,
        scope: PluginScope,
        grant: EnableGrant,
        host_version: &str,
        features: &BTreeSet<String>,
    ) -> Result<EnableRecord, BoundaryError> {
        scope.validate()?;
        let _lock = self.inner.disk.lock_index()?;
        let mut index = self.inner.read_index()?;
        let package = index.record(digest)?.clone();
        if package.state != InstallationState::Installed {
            return Err(error(
                "plugin_state",
                "Only installed plugin content can be enabled",
            ));
        }
        self.inner.load_verified(&package)?;
        package.manifest.check_host(host_version, features)?;
        grant.validate(&package.manifest)?;
        check_dependency_scope(&index, &package, &scope)?;
        let lock = package
            .lock
            .as_ref()
            .ok_or_else(|| error("plugin_corrupt", "Installed plugin has no dependency lock"))?;
        self.inner.verify_lock(&index, lock, None)?;
        check_lock_host(&index, lock, host_version, features)?;
        let revision = index.next_revision()?;
        let record = EnableRecord {
            plugin_id: package.manifest.plugin.id,
            digest: digest.into(),
            capability_fingerprint: package.capability_fingerprint,
            lock_fingerprint: package
                .lock_fingerprint
                .ok_or_else(|| error("plugin_corrupt", "Installed plugin has no lock identity"))?,
            scope,
            enabled: true,
            explicitly_disabled: false,
            revision,
            grant,
        };
        self.inner.check_metadata(&record)?;
        if let Some(existing) = index.enablements.iter_mut().find(|existing| {
            existing.plugin_id == record.plugin_id && existing.scope == record.scope
        }) {
            *existing = record.clone();
        } else {
            if index.enablements.len() >= MAX_ENABLEMENTS {
                return Err(error(
                    "plugin_limit",
                    "Plugin enablement capacity is exhausted",
                ));
            }
            index.enablements.push(record.clone());
        }
        self.inner.commit(&mut index)?;
        Ok(record)
    }

    /// Close new scheduling before the host cancels these owned runtime leases.
    /// Passing no scope disables every scope for this plugin ID.
    pub fn disable(
        &self,
        plugin_id: &str,
        scope: Option<&PluginScope>,
    ) -> Result<LifecycleChange, BoundaryError> {
        if let Some(scope) = scope {
            scope.validate()?;
        }
        let _lock = self.inner.disk.lock_index()?;
        let mut index = self.inner.read_index()?;
        let selected = index
            .selections
            .iter()
            .find(|selected| selected.plugin_id == plugin_id)
            .map(|selected| selected.digest.clone());
        let package = index
            .packages
            .iter()
            .find(|record| Some(record.digest.as_str()) == selected.as_deref())
            .or_else(|| {
                index
                    .packages
                    .iter()
                    .find(|record| record.manifest.plugin.id == plugin_id)
            })
            .cloned()
            .ok_or_else(|| error("plugin_not_found", "Plugin ID was not found"))?;
        for record in &mut index.enablements {
            if record.plugin_id == plugin_id && scope.is_none_or(|scope| *scope == record.scope) {
                record.enabled = false;
                record.explicitly_disabled = true;
            }
        }
        let tombstone_scope = scope.cloned().unwrap_or(PluginScope::User);
        if !index
            .enablements
            .iter()
            .any(|record| record.plugin_id == plugin_id && record.scope == tombstone_scope)
        {
            if index.enablements.len() >= MAX_ENABLEMENTS {
                return Err(error(
                    "plugin_limit",
                    "Plugin enablement capacity is exhausted",
                ));
            }
            let revision = index.next_revision()?;
            index.enablements.push(EnableRecord {
                plugin_id: plugin_id.into(),
                digest: package.digest.clone(),
                capability_fingerprint: package.capability_fingerprint.clone(),
                lock_fingerprint: package.lock_fingerprint.clone().unwrap_or_default(),
                scope: tombstone_scope,
                enabled: false,
                explicitly_disabled: true,
                revision,
                grant: EnableGrant::default(),
            });
        }
        let affected: BTreeSet<_> = index
            .packages
            .iter()
            .filter(|package| package.manifest.plugin.id == plugin_id)
            .map(|package| package.digest.as_str())
            .collect();
        let stopped: BTreeSet<_> = index
            .leases
            .iter()
            .filter(|lease| {
                affected.iter().any(|digest| lease_uses(lease, digest))
                    && effective_enablement(&index, plugin_id, &lease.scope)
                        .is_some_and(|enabled| enabled.explicitly_disabled)
            })
            .map(|lease| lease.lease_id.clone())
            .collect();
        for record in &mut index.leases {
            if stopped.contains(&record.lease_id) && record.state == InstanceState::Running {
                record.state = InstanceState::StopRequested;
            }
        }
        self.inner.reconcile_leases(&mut index)?;
        let cleanup_pending = index
            .leases
            .iter()
            .filter(|lease| {
                stopped.contains(&lease.lease_id) && lease.state != InstanceState::Cleaned
            })
            .cloned()
            .collect();
        self.inner.commit(&mut index)?;
        Ok(LifecycleChange {
            removed: false,
            cleanup_pending,
        })
    }

    pub fn acquire(
        &self,
        plugin_id: &str,
        workspace: &Path,
        session_id: SessionId,
        run_id: RunId,
        host_version: &str,
        features: &BTreeSet<String>,
    ) -> Result<PluginLease, BoundaryError> {
        let scope = PluginScope::workspace(workspace)?;
        let _lock = self.inner.disk.lock_index()?;
        let mut index = self.inner.read_index()?;
        self.inner.reconcile_leases(&mut index)?;
        let enabled = effective_enablement(&index, plugin_id, &scope)
            .filter(|record| record.enabled)
            .cloned()
            .ok_or_else(|| {
                error(
                    "plugin_disabled",
                    "Plugin is not enabled for this workspace",
                )
            })?;
        let package = index.record(&enabled.digest)?.clone();
        if package.state != InstallationState::Installed {
            return Err(error(
                "plugin_state",
                "Enabled plugin content is unavailable",
            ));
        }
        package.manifest.check_host(host_version, features)?;
        let lock = package
            .lock
            .as_ref()
            .ok_or_else(|| error("plugin_corrupt", "Enabled plugin has no dependency lock"))?;
        check_dependency_scope(&index, &package, &scope)?;
        self.inner.verify_lock(&index, lock, None)?;
        check_lock_host(&index, lock, host_version, features)?;
        let verified = self.inner.load_verified(&package)?;
        if index
            .leases
            .iter()
            .filter(|lease| lease.state != InstanceState::Cleaned)
            .count()
            >= self.inner.limits.max_leases
        {
            return Err(error("plugin_limit", "Plugin lease capacity is exhausted"));
        }
        index
            .leases
            .retain(|lease| lease.state != InstanceState::Cleaned);
        let record = LeaseRecord {
            lease_id: Uuid::now_v7().to_string(),
            plugin_id: plugin_id.into(),
            digest: enabled.digest,
            dependency_digests: lock
                .packages
                .iter()
                .filter(|dependency| dependency.digest != package.digest)
                .map(|dependency| dependency.digest.clone())
                .collect(),
            lock_fingerprint: enabled.lock_fingerprint,
            capability_fingerprint: enabled.capability_fingerprint,
            scope,
            enable_scope: enabled.scope,
            enable_revision: enabled.revision,
            session_id,
            run_id,
            grant: enabled.grant,
            state: InstanceState::Running,
        };
        let file = self.inner.disk.create_lease(&record.lease_id)?;
        index.leases.push(record.clone());
        self.inner.commit(&mut index)?;
        Ok(PluginLease::new(self.clone(), record, verified, file))
    }

    pub fn uninstall(&self, digest: &str) -> Result<LifecycleChange, BoundaryError> {
        let _lock = self.inner.disk.lock_index()?;
        let mut index = self.inner.read_index()?;
        index.record(digest)?;
        if index.packages.iter().any(|record| {
            record.digest != digest
                && record.state != InstallationState::UninstallRequested
                && record
                    .lock
                    .as_ref()
                    .is_some_and(|lock| lock.packages.iter().any(|pin| pin.digest == digest))
        }) {
            return Err(error(
                "plugin_dependency_in_use",
                "Another installed plugin lock still references this content",
            ));
        }
        index.record_mut(digest)?.state = InstallationState::UninstallRequested;
        index
            .selections
            .retain(|selected| selected.digest != digest);
        for record in &mut index.enablements {
            if record.digest == digest {
                record.enabled = false;
            }
        }
        for lease in &mut index.leases {
            if lease_uses(lease, digest) && lease.state == InstanceState::Running {
                lease.state = InstanceState::StopRequested;
            }
        }
        self.inner.reconcile_leases(&mut index)?;
        self.inner.commit(&mut index)?;
        self.inner.finish_uninstall(&mut index, digest)
    }

    /// Reconcile process-owned locks and finish safe removals. A crashed lease
    /// becomes Unknown; its content remains pinned until explicit confirmation.
    pub fn recover(&self) -> Result<Vec<LeaseRecord>, BoundaryError> {
        let _lock = self.inner.disk.lock_index()?;
        let mut index = self.inner.read_index()?;
        let changed = self.inner.reconcile_leases(&mut index)?;
        if changed {
            self.inner.commit(&mut index)?;
        }
        let removals: Vec<_> = index
            .packages
            .iter()
            .filter(|record| record.state == InstallationState::UninstallRequested)
            .map(|record| record.digest.clone())
            .collect();
        for digest in removals {
            self.inner.finish_uninstall(&mut index, &digest)?;
        }
        self.inner.disk.cleanup_staging()?;
        self.inner.disk.cleanup_unindexed(
            &index
                .packages
                .iter()
                .map(|record| record.digest.clone())
                .collect(),
        )?;
        self.inner.disk.cleanup_auxiliary(
            &index
                .leases
                .iter()
                .map(|lease| lease.lease_id.clone())
                .collect(),
        )?;
        Ok(index
            .leases
            .iter()
            .filter(|lease| lease.state != InstanceState::Cleaned)
            .cloned()
            .collect())
    }

    /// The trusted host must independently establish resource cleanup before
    /// resolving an orphaned lease. An active lease owner cannot be bypassed.
    pub fn confirm_cleanup(&self, lease_id: &str) -> Result<(), BoundaryError> {
        let _lock = self.inner.disk.lock_index()?;
        let mut index = self.inner.read_index()?;
        if self.inner.disk.lease_active(lease_id)? {
            return Err(error(
                "plugin_busy",
                "Plugin lease is still owned by a live process",
            ));
        }
        let lease = index
            .leases
            .iter_mut()
            .find(|lease| lease.lease_id == lease_id)
            .ok_or_else(|| error("plugin_not_found", "Plugin lease was not found"))?;
        if lease.state != InstanceState::Unknown && lease.state != InstanceState::Cleaned {
            return Err(error(
                "plugin_state",
                "Only an orphaned lease can be confirmed clean",
            ));
        }
        lease.state = InstanceState::Cleaned;
        self.inner.commit(&mut index)?;
        self.inner.disk.remove_lease(lease_id)
    }
}

impl Inner {
    fn check_metadata<T: Serialize>(&self, value: &T) -> Result<(), BoundaryError> {
        let encoded = serde_json::to_string(value)
            .map_err(|_| error("plugin_corrupt", "Plugin metadata cannot be encoded"))?;
        if crate::redaction::contains_secret(&encoded, &self.secrets.snapshot()?) {
            return Err(error(
                "sensitive_data",
                "Plugin metadata contains protected data",
            ));
        }
        Ok(())
    }
    fn check_manifest_secrets(&self, package: &VerifiedPackage) -> Result<(), BoundaryError> {
        let file = package
            .file(crate::plugin_manifest::MANIFEST_NAME)
            .ok_or_else(|| error("plugin_corrupt", "Sealed plugin manifest is missing"))?;
        let text = std::str::from_utf8(file.bytes())
            .map_err(|_| error("plugin_corrupt", "Sealed plugin manifest is not UTF-8"))?;
        crate::plugin_manifest::parse_manifest_with_secrets(text, &self.secrets.snapshot()?)?;
        Ok(())
    }
    fn read_index(&self) -> Result<Index, BoundaryError> {
        let index = self.disk.read_index()?.unwrap_or_else(Index::empty);
        self.validate_index(&index)?;
        Ok(index)
    }
    fn commit(&self, index: &mut Index) -> Result<(), BoundaryError> {
        index.revision = index.next_revision()?;
        self.validate_index(index)?;
        self.disk.write_index(index)
    }
    fn validate_index(&self, index: &Index) -> Result<(), BoundaryError> {
        if index.schema_version != 1
            || index.packages.len() > self.limits.max_objects
            || index.enablements.len() > MAX_ENABLEMENTS
            || index.leases.len() > self.limits.max_leases.saturating_mul(2)
        {
            return Err(error(
                "plugin_corrupt",
                "Plugin index version or capacities are invalid",
            ));
        }
        let mut contents = BTreeSet::new();
        let mut total = 0u64;
        for record in &index.packages {
            record.manifest.validate()?;
            record.source.validate()?;
            if !valid_digest(&record.digest)
                || !contents.insert(&record.digest)
                || record.capability_fingerprint != record.manifest.capability_fingerprint()?
                || record.bytes > crate::plugin_manifest::MAX_PACKAGE_BYTES
            {
                return Err(error("plugin_corrupt", "Plugin record identity is invalid"));
            }
            total = total
                .checked_add(record.bytes)
                .ok_or_else(|| error("plugin_corrupt", "Plugin stored byte count overflows"))?;
            if let Some(lock) = &record.lock {
                lock.validate()?;
                let text = lock.to_toml()?;
                if record.lock_fingerprint.as_deref()
                    != Some(digest_bytes(text.as_bytes()).as_str())
                {
                    return Err(error(
                        "plugin_corrupt",
                        "Plugin dependency lock identity differs",
                    ));
                }
                if !lock.packages.iter().any(|package| {
                    package.id == record.manifest.plugin.id
                        && package.digest == record.digest
                        && package.version == record.manifest.plugin.version
                        && package.capability_fingerprint == record.capability_fingerprint
                }) {
                    return Err(error(
                        "plugin_corrupt",
                        "Plugin dependency lock has no matching root",
                    ));
                }
            } else if record.state == InstallationState::Installed {
                return Err(error(
                    "plugin_corrupt",
                    "Installed plugin has no dependency lock",
                ));
            }
            self.check_metadata(record)?;
        }
        if total > self.limits.max_bytes {
            return Err(error(
                "plugin_limit",
                "Plugin store exceeds its configured capacity",
            ));
        }
        let mut selections = BTreeSet::new();
        for selection in &index.selections {
            let record = index.record(&selection.digest)?;
            if selection.plugin_id != record.manifest.plugin.id
                || !selections.insert(&selection.plugin_id)
            {
                return Err(error("plugin_corrupt", "Plugin selection is invalid"));
            }
        }
        let mut enabled = BTreeSet::new();
        for record in &index.enablements {
            record.scope.validate()?;
            if record.revision == 0
                || record.revision > index.revision
                || (record.enabled && record.explicitly_disabled)
                || !valid_digest(&record.digest)
                || !enabled.insert((record.plugin_id.as_str(), scope_key(&record.scope)?))
            {
                return Err(error(
                    "plugin_corrupt",
                    "Plugin enablement identity is invalid",
                ));
            }
            if record.enabled {
                let package = index.record(&record.digest)?;
                if package.state != InstallationState::Installed
                    || record.plugin_id != package.manifest.plugin.id
                    || record.capability_fingerprint != package.capability_fingerprint
                    || Some(record.lock_fingerprint.as_str()) != package.lock_fingerprint.as_deref()
                {
                    return Err(error(
                        "plugin_corrupt",
                        "Enabled plugin content identity changed",
                    ));
                }
                record.grant.validate(&package.manifest)?;
            }
            self.check_metadata(record)?;
        }
        let mut leases = BTreeSet::new();
        for lease in &index.leases {
            if Uuid::parse_str(&lease.lease_id).is_err()
                || !leases.insert(&lease.lease_id)
                || !valid_digest(&lease.digest)
                || lease.dependency_digests.len() > 128
            {
                return Err(error("plugin_corrupt", "Plugin lease identity is invalid"));
            }
            lease.scope.validate()?;
            lease.enable_scope.validate()?;
            if lease.state != InstanceState::Cleaned {
                let package = index.record(&lease.digest)?;
                if lease.plugin_id != package.manifest.plugin.id
                    || lease.capability_fingerprint != package.capability_fingerprint
                    || package.lock_fingerprint.as_deref() != Some(lease.lock_fingerprint.as_str())
                {
                    return Err(error(
                        "plugin_corrupt",
                        "Plugin lease content identity changed",
                    ));
                }
                lease.grant.validate(&package.manifest)?;
                let locked: BTreeSet<_> = package
                    .lock
                    .as_ref()
                    .ok_or_else(|| error("plugin_corrupt", "Leased plugin has no lock"))?
                    .packages
                    .iter()
                    .filter(|pin| pin.digest != package.digest)
                    .map(|pin| pin.digest.as_str())
                    .collect();
                if lease.enable_revision == 0
                    || lease.enable_revision > index.revision
                    || lease
                        .dependency_digests
                        .iter()
                        .map(String::as_str)
                        .collect::<BTreeSet<_>>()
                        != locked
                    || lease.dependency_digests.len() != locked.len()
                {
                    return Err(error(
                        "plugin_corrupt",
                        "Plugin lease dependency identity is invalid",
                    ));
                }
                for dependency in &lease.dependency_digests {
                    index.record(dependency)?;
                }
            }
            self.check_metadata(lease)?;
        }
        Ok(())
    }
    fn load_verified(&self, record: &PackageRecord) -> Result<VerifiedPackage, BoundaryError> {
        let package = self.disk.load_package(&record.digest)?;
        let bytes: u64 = package
            .files()
            .iter()
            .map(|file| file.bytes().len() as u64)
            .sum();
        if package.digest() != record.digest
            || package.manifest() != &record.manifest
            || package.capability_fingerprint() != record.capability_fingerprint
            || bytes != record.bytes
        {
            return Err(error(
                "plugin_corrupt",
                "Stored package bytes differ from their recorded identity",
            ));
        }
        if let Some(lock) = &record.lock {
            let text = self.disk.read_lock(&record.digest)?;
            if record.lock_fingerprint.as_deref() != Some(digest_bytes(&text).as_str())
                || PluginLock::parse(
                    std::str::from_utf8(&text)
                        .map_err(|_| error("plugin_corrupt", "Stored plugin lock is not UTF-8"))?,
                )? != *lock
            {
                return Err(error(
                    "plugin_corrupt",
                    "Stored plugin lock differs from its index identity",
                ));
            }
        }
        self.check_manifest_secrets(&package)?;
        Ok(package)
    }
    fn verify_lock(
        &self,
        index: &Index,
        lock: &PluginLock,
        staged: Option<&str>,
    ) -> Result<(), BoundaryError> {
        for pin in &lock.packages {
            let record = index.record(&pin.digest)?;
            if record.state != InstallationState::Installed
                && staged != Some(record.digest.as_str())
            {
                return Err(error("plugin_state", "Locked dependency is unavailable"));
            }
            if pin.source != record.source {
                return Err(error(
                    "plugin_corrupt",
                    "Locked dependency provenance differs from its installed record",
                ));
            }
            let package = self.load_verified(record)?;
            lock.verify_package(&package)?;
        }
        Ok(())
    }
    fn reconcile_leases(&self, index: &mut Index) -> Result<bool, BoundaryError> {
        let mut changed = false;
        for lease in &mut index.leases {
            if matches!(
                lease.state,
                InstanceState::Running | InstanceState::StopRequested
            ) && !self.disk.lease_active(&lease.lease_id)?
            {
                lease.state = InstanceState::Unknown;
                changed = true;
            }
        }
        Ok(changed)
    }
    fn finish_uninstall(
        &self,
        index: &mut Index,
        digest: &str,
    ) -> Result<LifecycleChange, BoundaryError> {
        let pending: Vec<_> = index
            .leases
            .iter()
            .filter(|lease| lease_uses(lease, digest) && lease.state != InstanceState::Cleaned)
            .cloned()
            .collect();
        if !pending.is_empty() {
            return Ok(LifecycleChange {
                removed: false,
                cleanup_pending: pending,
            });
        }
        self.disk.remove_object(digest)?;
        index.packages.retain(|record| record.digest != digest);
        index
            .selections
            .retain(|selected| selected.digest != digest);
        self.commit(index)?;
        Ok(LifecycleChange {
            removed: true,
            cleanup_pending: Vec::new(),
        })
    }
}
fn scope_key(scope: &PluginScope) -> Result<String, BoundaryError> {
    Ok(digest(&serde_json::to_vec(scope).map_err(|_| {
        error("plugin_scope", "Plugin scope cannot be encoded")
    })?))
}
fn effective_enablement<'a>(
    index: &'a Index,
    id: &str,
    workspace: &PluginScope,
) -> Option<&'a EnableRecord> {
    index
        .enablements
        .iter()
        .find(|record| record.plugin_id == id && &record.scope == workspace)
        .or_else(|| {
            index
                .enablements
                .iter()
                .find(|record| record.plugin_id == id && record.scope == PluginScope::User)
        })
}
fn lease_uses(lease: &LeaseRecord, digest: &str) -> bool {
    lease.digest == digest
        || lease
            .dependency_digests
            .iter()
            .any(|dependency| dependency == digest)
}
fn check_dependency_scope(
    index: &Index,
    package: &PackageRecord,
    workspace: &PluginScope,
) -> Result<(), BoundaryError> {
    let lock = package
        .lock
        .as_ref()
        .ok_or_else(|| error("plugin_corrupt", "Plugin dependency lock is missing"))?;
    if lock
        .packages
        .iter()
        .filter(|dependency| dependency.digest != package.digest)
        .any(|dependency| {
            effective_enablement(index, &dependency.id, workspace)
                .is_some_and(|enabled| enabled.explicitly_disabled)
        })
    {
        return Err(error(
            "plugin_disabled",
            "A plugin dependency is explicitly disabled in this scope",
        ));
    }
    Ok(())
}
fn digest_bytes(bytes: &[u8]) -> String {
    digest(bytes)
}
fn summary(index: &Index, record: &PackageRecord) -> PluginSummary {
    let id = &record.manifest.plugin.id;
    PluginSummary {
        plugin_id: id.clone(),
        version: record.manifest.plugin.version.clone(),
        digest: record.digest.clone(),
        source: record.source.clone(),
        capability_fingerprint: record.capability_fingerprint.clone(),
        requested_capabilities: record.manifest.capabilities.clone(),
        state: record.state,
        selected: index
            .selections
            .iter()
            .any(|selection| selection.plugin_id == *id && selection.digest == record.digest),
        source_conflict: index.packages.iter().any(|other| {
            other.manifest.plugin.id == *id
                && other.manifest.plugin.version == record.manifest.plugin.version
                && other.digest != record.digest
        }),
        enabled_scopes: index
            .enablements
            .iter()
            .filter(|enabled| enabled.digest == record.digest)
            .cloned()
            .collect(),
        instances: index
            .leases
            .iter()
            .filter(|lease| lease_uses(lease, &record.digest))
            .cloned()
            .collect(),
    }
}
fn build_lock(index: &Index, root: &PackageRecord) -> Result<PluginLock, BoundaryError> {
    use crate::plugin_manifest::{DependencyPin, LockedPackage, TREE_DIGEST_ALGORITHM};
    let mut packages: BTreeMap<String, LockedPackage> = BTreeMap::new();
    let mut pins = Vec::new();
    for dependency in &root.manifest.dependencies {
        let selected = index
            .selections
            .iter()
            .find(|selected| selected.plugin_id == dependency.id)
            .ok_or_else(|| {
                error(
                    "plugin_dependency_missing",
                    "Plugin dependency has no selected installed version",
                )
            })?;
        let record = index.record(&selected.digest)?;
        if record.state != InstallationState::Installed
            || !semver::VersionReq::parse(&dependency.version)
                .map_err(|_| error("plugin_corrupt", "Plugin dependency range is invalid"))?
                .matches(
                    &semver::Version::parse(&record.manifest.plugin.version).map_err(|_| {
                        error("plugin_corrupt", "Installed dependency version is invalid")
                    })?,
                )
        {
            return Err(error(
                "plugin_dependency_missing",
                "Selected dependency does not satisfy its version range",
            ));
        }
        let lock = record
            .lock
            .as_ref()
            .ok_or_else(|| error("plugin_corrupt", "Installed dependency has no lock"))?;
        for pin in &lock.packages {
            if packages
                .get(&pin.id)
                .is_some_and(|existing| existing.digest != pin.digest)
            {
                return Err(error(
                    "plugin_source_conflict",
                    "Dependency closure selects conflicting plugin content",
                ));
            }
            packages.insert(pin.id.clone(), pin.clone());
        }
        pins.push(DependencyPin {
            id: dependency.id.clone(),
            version: record.manifest.plugin.version.clone(),
            digest: record.digest.clone(),
        });
    }
    let id = root.manifest.plugin.id.clone();
    if packages.contains_key(&id) {
        return Err(error(
            "invalid_plugin",
            "Plugin dependency closure contains a cycle",
        ));
    }
    packages.insert(
        id.clone(),
        LockedPackage {
            id,
            version: root.manifest.plugin.version.clone(),
            source: root.source.clone(),
            digest: root.digest.clone(),
            capability_fingerprint: root.capability_fingerprint.clone(),
            dependencies: pins,
        },
    );
    let lock = PluginLock {
        schema_version: 1,
        digest_algorithm: TREE_DIGEST_ALGORITHM.into(),
        packages: packages.into_values().collect(),
        aliases: Vec::new(),
    };
    lock.validate()?;
    Ok(lock)
}
fn check_lock_host(
    index: &Index,
    lock: &PluginLock,
    host_version: &str,
    features: &BTreeSet<String>,
) -> Result<(), BoundaryError> {
    for pin in &lock.packages {
        index
            .record(&pin.digest)?
            .manifest
            .check_host(host_version, features)?;
    }
    Ok(())
}
