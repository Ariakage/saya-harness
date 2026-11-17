//! Explicit sandbox policy settings. Resolution never probes or launches a backend.

use super::{Environment, invalid};
use crate::{
    contract::BoundaryError,
    execution::{BackendKind, ResourceLimits, SandboxPolicy},
    network::{BrokerLimits, BrokerPolicy},
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Explicit read-only toolchain and environment presets for sandboxed execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolchainPreset {
    /// Enable all host toolchain presets that exist locally plus minimal Linux NSS files.
    Auto,
    /// Read-only Rustup and Cargo directories (`RUSTUP_HOME`, `CARGO_HOME`, `~/.rustup`, `~/.cargo`).
    Rust,
    /// Read-only Python toolchain directories (`PYENV_ROOT`, `UV_PYTHON_INSTALL_DIR`, `VIRTUAL_ENV`, `~/.pyenv`, `~/.local/share/uv`).
    Python,
    /// Read-only Node.js manager directories (`NVM_DIR`, `FNM_DIR`, `VOLTA_HOME`, `PNPM_HOME`).
    Node,
    /// Read-only Homebrew prefixes (macOS `/opt/homebrew`, `/usr/local/{Homebrew,Cellar,opt,bin}` or Linuxbrew).
    Homebrew,
    /// Read-only macOS Xcode and Command Line Tools SDK directories.
    XcodeClt,
    /// Minimal synthetic `/etc/{passwd,group,nsswitch.conf}` identity entries on Linux.
    MinimalEtc,
}

fn is_false(value: &bool) -> bool {
    !*value
}

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
    /// Expose existing system CA certificate directories read-only when true.
    #[serde(default, skip_serializing_if = "is_false")]
    pub ca_certs: bool,
    /// Explicit read-only toolchain presets resolved lazily per run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub toolchain_presets: Vec<ToolchainPreset>,
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
                public_web: false,
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
        if self.toolchain_presets.len() > 32 {
            return Err(invalid("sandbox toolchain preset count exceeds its limit"));
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
        self.resolve_with_environment(workspace, &Environment::capture())
    }

    /// Build policy data using an explicit host environment snapshot for toolchain preset lookup.
    pub fn resolve_with_environment(
        &self,
        workspace: &Path,
        env: &Environment,
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
        self.apply_presets(&mut policy, env);
        Ok((policy, self.network.broker()))
    }

    fn apply_presets(&self, policy: &mut SandboxPolicy, env: &Environment) {
        if !self.ca_certs && self.toolchain_presets.is_empty() {
            return;
        }
        let home = env.path("HOME").filter(|path| path.is_absolute());
        let home_canonical = home.as_deref().and_then(|path| fs::canonicalize(path).ok());
        let mut add_dir = |candidate: &Path| {
            push_safe_read_dir(
                &mut policy.read_roots,
                candidate,
                home.as_deref(),
                home_canonical.as_deref(),
            );
        };
        if self.ca_certs {
            let cert_dirs: &[&str] = if cfg!(target_os = "macos") {
                &["/etc/ssl"]
            } else {
                &["/etc/ssl", "/etc/pki", "/etc/ca-certificates"]
            };
            for path in cert_dirs {
                add_dir(Path::new(path));
            }
        }
        let mut expand = Vec::new();
        for preset in &self.toolchain_presets {
            if *preset == ToolchainPreset::Auto {
                expand.extend([
                    ToolchainPreset::Rust,
                    ToolchainPreset::Python,
                    ToolchainPreset::Node,
                    ToolchainPreset::Homebrew,
                    ToolchainPreset::XcodeClt,
                    ToolchainPreset::MinimalEtc,
                ]);
            } else {
                expand.push(*preset);
            }
        }
        for preset in expand {
            match preset {
                ToolchainPreset::Auto => {}
                ToolchainPreset::Rust => {
                    policy.synthetic_etc_identity = true;
                    for key in ["RUSTUP_HOME", "CARGO_HOME"] {
                        if let Some(dir) = env.path(key) {
                            add_dir(&dir);
                        }
                    }
                    if let Some(home_dir) = home.as_deref() {
                        for rel in [".rustup", ".cargo"] {
                            add_dir(&home_dir.join(rel));
                        }
                    }
                }
                ToolchainPreset::Python => {
                    policy.synthetic_etc_identity = true;
                    for key in ["PYENV_ROOT", "UV_PYTHON_INSTALL_DIR", "VIRTUAL_ENV"] {
                        if let Some(dir) = env.path(key) {
                            add_dir(&dir);
                        }
                    }
                    if let Some(home_dir) = home.as_deref() {
                        for rel in [".pyenv", ".local/share/uv"] {
                            add_dir(&home_dir.join(rel));
                        }
                    }
                    if cfg!(target_os = "macos") {
                        add_dir(Path::new("/Library/Frameworks/Python.framework"));
                    }
                }
                ToolchainPreset::Node => {
                    policy.synthetic_etc_identity = true;
                    for key in ["NVM_DIR", "FNM_DIR", "VOLTA_HOME", "PNPM_HOME"] {
                        if let Some(dir) = env.path(key) {
                            add_dir(&dir);
                        }
                    }
                    if let Some(home_dir) = home.as_deref() {
                        for rel in [".nvm", ".local/share/fnm", ".volta", ".local/share/pnpm"] {
                            add_dir(&home_dir.join(rel));
                        }
                    }
                }
                ToolchainPreset::Homebrew => {
                    let dirs: &[&str] = if cfg!(target_os = "macos") {
                        &[
                            "/opt/homebrew",
                            "/usr/local/Homebrew",
                            "/usr/local/Cellar",
                            "/usr/local/opt",
                            "/usr/local/bin",
                        ]
                    } else {
                        &["/home/linuxbrew/.linuxbrew"]
                    };
                    for dir in dirs {
                        add_dir(Path::new(dir));
                    }
                }
                ToolchainPreset::XcodeClt => {
                    if cfg!(target_os = "macos") {
                        for dir in [
                            "/Library/Developer/CommandLineTools",
                            "/Applications/Xcode.app/Contents/Developer",
                        ] {
                            add_dir(Path::new(dir));
                        }
                    }
                }
                ToolchainPreset::MinimalEtc => {
                    policy.synthetic_etc_identity = true;
                }
            }
        }
    }
}

fn push_safe_read_dir(
    roots: &mut Vec<PathBuf>,
    candidate: &Path,
    home: Option<&Path>,
    home_canonical: Option<&Path>,
) {
    if !candidate.is_absolute() || !valid_path(candidate) || !candidate.is_dir() {
        return;
    }
    if candidate == Path::new("/")
        || home.is_some_and(|h| candidate == h)
        || candidate
            .components()
            .any(|c| c.as_os_str() == ".saya" || c.as_os_str() == ".soyo")
    {
        return;
    }
    if let Ok(canon) = fs::canonicalize(candidate)
        && (canon == Path::new("/")
            || home_canonical.is_some_and(|h| canon == h)
            || canon
                .components()
                .any(|c| c.as_os_str() == ".saya" || c.as_os_str() == ".soyo"))
    {
        return;
    }
    if !roots.iter().any(|existing| existing == candidate) {
        roots.push(candidate.to_path_buf());
    }
}

fn valid_path(path: &Path) -> bool {
    path.to_str().is_some_and(|path| {
        !path.is_empty() && path.len() <= 16 * 1024 && !path.chars().any(char::is_control)
    })
}
