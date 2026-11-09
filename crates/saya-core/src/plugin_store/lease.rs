use super::*;
use std::fs::File;

/// A runtime pin for one immutable package and its fixed dependency closure.
/// The host owns cancellation and actual resource cleanup. Dropping a lease
/// releases its process lock; recovery records Unknown until cleanup is proven.
pub struct PluginLease {
    store: PluginStore,
    record: LeaseRecord,
    package: VerifiedPackage,
    _owner: File,
}
impl std::fmt::Debug for PluginLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PluginLease")
            .field("lease_id", &self.record.lease_id)
            .field("digest", &self.record.digest)
            .finish_non_exhaustive()
    }
}
impl PluginLease {
    pub(super) fn new(
        store: PluginStore,
        record: LeaseRecord,
        package: VerifiedPackage,
        file: File,
    ) -> Self {
        Self {
            store,
            record,
            package,
            _owner: file,
        }
    }
    pub fn record(&self) -> &LeaseRecord {
        &self.record
    }
    pub fn package(&self) -> &VerifiedPackage {
        &self.package
    }
    /// This path is for a host-approved read-only mount. The private store root
    /// must remain inaccessible to the worker. Verify contents at worker entry.
    pub fn package_path(&self) -> PathBuf {
        self.store.inner.disk.package_path(&self.record.digest)
    }

    /// Check durable scheduling revocation before another operation. Existing
    /// leases retain their original grant across a selected-version update.
    pub fn check_active(&self) -> Result<(), BoundaryError> {
        let _lock = self.store.inner.disk.lock_index()?;
        let index = self.store.inner.read_index()?;
        self.active_record(&index)?;
        Ok(())
    }
    /// Revalidate fixed package and dependency bytes before a new worker entry.
    /// Current permission, mode, backend and credential checks are host-owned.
    pub fn verify_contents(&self) -> Result<(), BoundaryError> {
        let _lock = self.store.inner.disk.lock_index()?;
        let index = self.store.inner.read_index()?;
        self.active_record(&index)?;
        let package = index.record(&self.record.digest)?;
        if package.state != InstallationState::Installed {
            return Err(error(
                "plugin_state",
                "Leased plugin content is unavailable",
            ));
        }
        let lock = package
            .lock
            .as_ref()
            .ok_or_else(|| error("plugin_corrupt", "Leased plugin has no dependency lock"))?;
        self.store.inner.verify_lock(&index, lock, None)
    }
    fn active_record<'a>(&self, index: &'a Index) -> Result<&'a LeaseRecord, BoundaryError> {
        let actual = index
            .leases
            .iter()
            .find(|lease| lease.lease_id == self.record.lease_id)
            .ok_or_else(|| error("plugin_revoked", "Plugin lease no longer exists"))?;
        if actual.state != InstanceState::Running || actual != &self.record {
            return Err(error(
                "plugin_revoked",
                "Plugin lease is stopped or its identity changed",
            ));
        }
        check_dependency_scope(index, index.record(&actual.digest)?, &actual.scope)?;
        Ok(actual)
    }

    /// Publish the host's observed cleanup result before releasing ownership.
    /// Unknown retains the immutable content pin for explicit reconciliation.
    pub fn finish(self, outcome: CleanupOutcome) -> Result<(), BoundaryError> {
        let _lock = self.store.inner.disk.lock_index()?;
        let mut index = self.store.inner.read_index()?;
        let lease = index
            .leases
            .iter_mut()
            .find(|lease| lease.lease_id == self.record.lease_id)
            .ok_or_else(|| error("plugin_not_found", "Plugin lease was not found"))?;
        let mut original = lease.clone();
        original.state = InstanceState::Running;
        if original != self.record {
            return Err(error("plugin_corrupt", "Plugin lease identity changed"));
        }
        lease.state = match outcome {
            CleanupOutcome::Cleaned => InstanceState::Cleaned,
            CleanupOutcome::Unknown => InstanceState::Unknown,
        };
        self.store.inner.commit(&mut index)?;
        if outcome == CleanupOutcome::Cleaned {
            self.store.inner.disk.remove_lease(&self.record.lease_id)?;
        }
        Ok(())
    }
}
