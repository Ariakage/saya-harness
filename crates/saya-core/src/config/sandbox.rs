//! Explicit sandbox policy settings. Resolution never probes or launches a backend.

use super::invalid;
use crate::{
    contract::BoundaryError,
    execution::{BackendKind, ResourceLimits, SandboxPolicy},
    network::{BrokerLimits, BrokerPolicy},
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Host-selected filesystem, resource and network constraints for one Run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SandboxSettings {
    pub backend: BackendKind,
    /// Additional readable roots, resolved relative to the workspace.
    pub read_roots: Vec<PathBuf>,
    /// Additional writable roots, resolved relative to the workspace.
    pub write_roots: Vec<PathBuf>,
    pub protected_paths: Vec<PathBuf>,
    pub resources: ResourceLimits,
    /// An explicitly delegated, absolute cgroup v2 directory.
    pub cgroup_root: Option<PathBuf>,
    pub network: SandboxNetworkSettings,
}

/// A per-Run broker receives its listener addresses from the host at startup.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum SandboxNetworkSettings {
    Deny {},
    Proxy {
        #[serde(default)]
        allowed_origins: Vec<String>,
        #[serde(default)]
        localhost_development_origin: Option<String>,
        #[serde(default)]
        limits: BrokerLimits,
    },
}
impl Default for SandboxNetworkSettings {
    fn default() -> Self {
        Self::Deny {}
    }
}
impl SandboxNetworkSettings {
    fn broker(&self) -> Option<BrokerPolicy> {
        match self {
            Self::Deny {} => None,
            Self::Proxy {
                allowed_origins,
                localhost_development_origin,
                limits,
            } => Some(BrokerPolicy {
                allowed_origins: allowed_origins.clone(),
                localhost_development_origin: localhost_development_origin.clone(),
                unix_socket: None,
                limits: limits.clone(),
            }),
        }
    }
}

impl SandboxSettings {
    /// Validate bounded policy syntax without filesystem changes, DNS or backend probes.
    /// Actual path identities and mandatory capabilities are checked by backend preparation.
    pub fn validate(&self) -> Result<(), BoundaryError> {
        for paths in [&self.read_roots, &self.write_roots, &self.protected_paths] {
            if paths.len() > 128 || paths.iter().any(|path| !valid_path(path)) {
                return Err(invalid("sandbox paths or root counts are invalid"));
            }
        }
        if self
            .cgroup_root
            .as_ref()
            .is_some_and(|path| !path.is_absolute() || !valid_path(path))
        {
            return Err(invalid("sandbox cgroup root must be an absolute path"));
        }
        self.resources
            .validate()
            .map_err(|_| invalid("sandbox resource limits are invalid"))?;
        if matches!(
            &self.network,
            SandboxNetworkSettings::Proxy { allowed_origins, .. } if allowed_origins.len() > 256
        ) {
            return Err(invalid("sandbox network origin count exceeds its limit"));
        }
        if let Some(broker) = self.network.broker() {
            broker
                .validate()
                .map_err(|_| invalid("sandbox network policy is invalid"))?;
        }
        Ok(())
    }

    /// Build policy data for an absolute workspace. Relative configured paths use that workspace.
    /// The caller adds application-protected paths and permission roots before preparation.
    pub fn resolve(
        &self,
        workspace: &Path,
    ) -> Result<(SandboxPolicy, Option<BrokerPolicy>), BoundaryError> {
        self.validate()?;
        if !workspace.is_absolute() || !valid_path(workspace) {
            return Err(invalid("sandbox workspace must be an absolute path"));
        }
        let mut policy = SandboxPolicy::workspace(workspace.to_path_buf());
        policy.backend = self.backend;
        policy
            .read_roots
            .extend(self.read_roots.iter().map(|path| workspace.join(path)));
        policy
            .write_roots
            .extend(self.write_roots.iter().map(|path| workspace.join(path)));
        policy.protected_paths = self
            .protected_paths
            .iter()
            .map(|path| workspace.join(path))
            .collect();
        policy.resources = self.resources.clone();
        policy.cgroup_root = self.cgroup_root.clone();
        Ok((policy, self.network.broker()))
    }
}

fn valid_path(path: &Path) -> bool {
    path.to_str().is_some_and(|path| {
        !path.is_empty() && path.len() <= 16 * 1024 && !path.chars().any(char::is_control)
    })
}
