//! Dedicated-VM browser worker backend.
//!
//! macOS Seatbelt cannot keep an outer sandbox while Chromium reinitializes
//! its own sandbox, and the Linux Bubblewrap profile used by the native
//! browser backend cannot satisfy Chromium's inner namespace setup. This module
//! keeps both sandboxes intact by running the pinned Playwright worker and
//! Chromium inside an explicitly provisioned Colima VM while the host only
//! runs a strictly confined `ssh` client.
//!
//! The VM is a host-provisioned input. Nothing here creates, installs or
//! updates it. The host SSH process is prepared through the existing
//! `PreparedEnvironment`/`ManagedProcess` boundary and may reach only the VM's
//! loopback SSH port. Its writable scope is a run-private directory; the SSH
//! identity, known-hosts file and binary are exposed read-only.
//!
//! VM network access for page traffic is not provided in this increment. The
//! backend never treats the VM's default egress as a controlled browser exit.

use super::{
    BrowserFailure,
    network::{
        BrowserNetwork, BrowserNetworkLog, BrowserNetworkSettings, MAX_NETWORK_OBSERVATIONS,
    },
    worker::{
        self, BrowserEnvironmentFactory, BrowserWorkspaceLease, CHROMIUM_REVISION,
        CHROMIUM_VERSION, ChromiumSandboxEvidence, ChromiumSandboxInspector, IdentityManifest,
        MAX_IDENTITY_FILE_BYTES, MAX_MANIFEST_BYTES, PLAYWRIGHT_MCP_VERSION, PLAYWRIGHT_VERSION,
        VerifiedWorkerIdentity, WorkerConnection, WorkerIdentityVerifier, WorkerLaunchPlanner,
        WorkerLaunchRequest, WorkerLauncher, parse_browsers, parse_package, sha256_hex,
        valid_sha256,
    },
};
use crate::{
    boundary::Cancellation,
    contract::{BoundaryError, ErrorInfo},
    execution::{
        self, BrowserSandboxDirectories, NetworkPolicy, PreparedEnvironment, SandboxPolicy,
    },
    mcp::{McpCatalog, McpLimits, McpToolResult},
    network::RunNetworkBudget,
    tools::exec::{ExecConfig, ExecPlan, ManagedProcess},
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    ffi::{OsStr, OsString},
    fs::{self, DirBuilder, File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    sync::Mutex as AsyncMutex,
};

/// The only supported backend. Colima owns an isolated profile-based VM.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserVmBackend {
    #[default]
    Colima,
}

/// Strict, default-disabled VM worker configuration. Every path and identity
/// is an explicit host input. The host `ssh` and `colima` programs are never
/// discovered through `PATH`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct BrowserVmSettings {
    pub enabled: bool,
    pub backend: BrowserVmBackend,
    /// Absolute path to the `colima` executable used by the provisioning tool.
    pub colima: Option<PathBuf>,
    /// Absolute path to the host `ssh` executable.
    pub ssh: Option<PathBuf>,
    /// Existing dedicated Colima profile name.
    pub profile: Option<String>,
    /// Literal loopback address where Colima forwards the VM SSH port.
    pub host: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    /// Absolute path to the private key. The file is never read by this crate;
    /// it is referenced by a generated strict SSH configuration.
    pub identity_file: Option<PathBuf>,
    /// Absolute path to a host-known-host file with the VM host key.
    pub known_hosts: Option<PathBuf>,
    /// Remote POSIX absolute paths pinned by the provisioning manifest.
    pub node: Option<String>,
    pub worker_root: Option<String>,
    pub chromium: Option<String>,
    pub identity_manifest: Option<String>,
    pub identity_sha256: Option<String>,
    /// Remote parent for unique 0700 Run roots.
    pub remote_temp_root: Option<String>,
    pub connect_timeout_ms: u64,
    pub request_timeout_ms: u64,
    pub cleanup_timeout_ms: u64,
    pub stderr_limit_bytes: usize,
}

impl Default for BrowserVmSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            backend: BrowserVmBackend::Colima,
            colima: None,
            ssh: None,
            profile: None,
            host: None,
            user: None,
            port: None,
            identity_file: None,
            known_hosts: None,
            node: None,
            worker_root: None,
            chromium: None,
            identity_manifest: None,
            identity_sha256: None,
            remote_temp_root: None,
            connect_timeout_ms: 20_000,
            request_timeout_ms: 120_000,
            cleanup_timeout_ms: 10_000,
            stderr_limit_bytes: 64 * 1024,
        }
    }
}

/// Canonical local inputs used by the host SSH channel.
#[derive(Debug, Clone)]
pub struct VmLocalPaths {
    pub ssh: PathBuf,
    pub identity_file: PathBuf,
    pub known_hosts: PathBuf,
    pub colima: PathBuf,
}

fn invalid_config(message: &'static str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: "invalid_config".into(),
            message: message.into(),
        },
        None,
        None,
    )
}

fn vm_unavailable(message: &'static str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: "browser_vm_unavailable".into(),
            message: message.into(),
        },
        None,
        None,
    )
}

fn identity_failure(message: impl Into<String>) -> BrowserFailure {
    BrowserFailure::new("worker_identity", message)
}

fn remote_failure(message: impl Into<String>) -> BrowserFailure {
    BrowserFailure::new("browser_vm_transport", message)
}

fn remote_cleanup_failure(message: impl Into<String>) -> BrowserFailure {
    BrowserFailure::uncertain(message)
}

fn safe_token(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value.len() <= max
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        && !value.starts_with('-')
}

/// Remote paths become part of a fixed remote shell script. Reject quoting and
/// shell metacharacters outright instead of relying on quoting alone.
fn safe_remote_path(value: &str) -> bool {
    value.starts_with('/')
        && value.len() <= 4096
        && !value.chars().any(char::is_control)
        && !value.split('/').any(|component| component == "..")
        && !value.chars().any(|ch| {
            matches!(
                ch,
                '\\' | '\''
                    | '"'
                    | ';'
                    | '|'
                    | '&'
                    | '$'
                    | '`'
                    | '<'
                    | '>'
                    | '('
                    | ')'
                    | '{'
                    | '}'
                    | '['
                    | ']'
                    | '*'
                    | '?'
                    | '!'
                    | '#'
                    | '~'
                    | '\n'
                    | '\r'
                    | '\t'
            )
        })
}

fn safe_local_path(path: &Path) -> bool {
    path.is_absolute()
        && path.to_str().is_some_and(|value| {
            !value.is_empty()
                && value.len() <= 4096
                && !value.chars().any(|ch| {
                    ch.is_control() || ch.is_whitespace() || matches!(ch, '"' | '\'' | '\\')
                })
        })
}

/// Normalize a literal loopback address to the bare form accepted by `ssh`
/// and the resolver. Bracketed IPv6 (`[::1]`) is URL display syntax; it is
/// never handed to `ssh`, `HostName` or the network policy as-is.
fn normalized_loopback_host(value: &str) -> Option<String> {
    if value.is_empty() || value.trim() != value {
        return None;
    }
    let bare = if let Some(inner) = value.strip_prefix('[') {
        inner.strip_suffix(']')?
    } else {
        value
    };
    if bare.is_empty() || bare.contains('[') || bare.contains(']') {
        return None;
    }
    bare.parse::<std::net::IpAddr>()
        .ok()
        .filter(std::net::IpAddr::is_loopback)
        .map(|address| address.to_string())
}

fn loopback_host(value: &str) -> bool {
    normalized_loopback_host(value).is_some()
}

impl BrowserVmSettings {
    /// Validate bounded syntax without touching the host or the VM.
    pub fn validate(&self) -> Result<(), BoundaryError> {
        if !matches!(self.backend, BrowserVmBackend::Colima) {
            return Err(invalid_config("Browser VM backend must be colima"));
        }
        for path in [
            &self.colima,
            &self.ssh,
            &self.identity_file,
            &self.known_hosts,
        ]
        .into_iter()
        .flatten()
        {
            if !safe_local_path(path) {
                return Err(invalid_config(
                    "Browser VM host paths must be absolute paths without whitespace or quoting characters",
                ));
            }
        }
        if self
            .profile
            .as_deref()
            .is_some_and(|profile| !safe_token(profile, 64))
        {
            return Err(invalid_config(
                "Browser VM profile must be an existing simple Colima profile name",
            ));
        }
        if self
            .user
            .as_deref()
            .is_some_and(|user| !safe_token(user, 32))
        {
            return Err(invalid_config("Browser VM SSH user is invalid"));
        }
        if self
            .host
            .as_deref()
            .is_some_and(|host| host.is_empty() || host.len() > 255 || !loopback_host(host))
        {
            return Err(invalid_config(
                "Browser VM SSH host must be a literal loopback address",
            ));
        }
        if self.port.is_some_and(|port| port == 0) {
            return Err(invalid_config("Browser VM SSH port must be nonzero"));
        }
        for path in [
            &self.node,
            &self.worker_root,
            &self.chromium,
            &self.identity_manifest,
            &self.remote_temp_root,
        ]
        .into_iter()
        .flatten()
        {
            if !safe_remote_path(path) {
                return Err(invalid_config(
                    "Browser VM remote paths must be safe absolute POSIX paths",
                ));
            }
        }
        if self.remote_temp_root.as_deref() == Some("/") {
            return Err(invalid_config(
                "Browser VM remote temp root must be narrower than the filesystem root",
            ));
        }
        if let Some(digest) = &self.identity_sha256
            && !valid_sha256(digest)
        {
            return Err(invalid_config(
                "Browser VM identity digest must be lowercase SHA-256",
            ));
        }
        if !(1_000..=120_000).contains(&self.connect_timeout_ms)
            || !(1_000..=600_000).contains(&self.request_timeout_ms)
            || !(1_000..=120_000).contains(&self.cleanup_timeout_ms)
            || !(4 * 1024..=1024 * 1024).contains(&self.stderr_limit_bytes)
        {
            return Err(invalid_config("Browser VM limits are invalid"));
        }
        if self.enabled {
            let required = [
                self.colima.is_some(),
                self.ssh.is_some(),
                self.profile.is_some(),
                self.host.is_some(),
                self.user.is_some(),
                self.port.is_some(),
                self.identity_file.is_some(),
                self.known_hosts.is_some(),
                self.node.is_some(),
                self.worker_root.is_some(),
                self.chromium.is_some(),
                self.identity_manifest.is_some(),
                self.identity_sha256.is_some(),
                self.remote_temp_root.is_some(),
            ];
            if required.iter().any(|present| !present) {
                return Err(invalid_config(
                    "Enabled Browser VM requires colima, ssh, profile, host, user, port, identity, known-hosts, remote identity paths and remote temp root",
                ));
            }
        }
        Ok(())
    }

    /// Validate that every local connection input exists and is permissioned
    /// for the host channel. Remote identity is verified at launch time.
    pub fn preflight_local(&self) -> Result<VmLocalPaths, BoundaryError> {
        self.validate()?;
        if !self.enabled {
            return Err(invalid_config("Browser VM backend is disabled"));
        }
        if !cfg!(any(target_os = "macos", target_os = "linux")) {
            return Err(vm_unavailable(
                "the Colima browser VM backend requires macOS or Linux native isolation",
            ));
        }
        let ssh = self
            .ssh
            .as_ref()
            .ok_or_else(|| invalid_config("Browser VM ssh path is missing"))?;
        let identity_file = self
            .identity_file
            .as_ref()
            .ok_or_else(|| invalid_config("Browser VM identity file is missing"))?;
        let known_hosts = self
            .known_hosts
            .as_ref()
            .ok_or_else(|| invalid_config("Browser VM known-hosts file is missing"))?;
        let colima = self
            .colima
            .as_ref()
            .ok_or_else(|| invalid_config("Browser VM colima path is missing"))?;

        let ssh = canonical_executable(ssh, "Browser VM ssh executable is unavailable")?;
        let colima = canonical_executable(colima, "Browser VM colima executable is unavailable")?;
        let identity_file = canonical_private_file(
            identity_file,
            "Browser VM identity file is unavailable or too permissive",
        )?;
        let known_hosts = canonical_read_file(
            known_hosts,
            "Browser VM known-hosts file is unavailable or writable",
        )?;
        Ok(VmLocalPaths {
            ssh,
            identity_file,
            known_hosts,
            colima,
        })
    }

    fn resolved_remote_paths(&self) -> Result<RemotePaths, BoundaryError> {
        Ok(RemotePaths {
            node: require_remote(&self.node, "node")?,
            worker_root: require_remote(&self.worker_root, "worker root")?,
            chromium: require_remote(&self.chromium, "Chromium")?,
            identity_manifest: require_remote(&self.identity_manifest, "identity manifest")?,
            identity_sha256: self
                .identity_sha256
                .clone()
                .ok_or_else(|| invalid_config("Browser VM identity digest is missing"))?,
        })
    }
}

fn require_remote(value: &Option<String>, name: &'static str) -> Result<String, BoundaryError> {
    value.clone().ok_or_else(|| {
        invalid_config(match name {
            "node" => "Browser VM node path is missing",
            "worker root" => "Browser VM worker root is missing",
            "Chromium" => "Browser VM Chromium path is missing",
            "identity manifest" => "Browser VM identity manifest is missing",
            "remote temp root" => "Browser VM remote temp root is missing",
            _ => "Browser VM remote path is missing",
        })
    })
}

#[derive(Debug, Clone)]
struct RemotePaths {
    node: String,
    worker_root: String,
    chromium: String,
    identity_manifest: String,
    identity_sha256: String,
}

fn canonical_executable(path: &Path, message: &'static str) -> Result<PathBuf, BoundaryError> {
    let path = fs::canonicalize(path).map_err(|_| invalid_config(message))?;
    require_safe_canonical_path(&path, message)?;
    let metadata = fs::metadata(&path).map_err(|_| invalid_config(message))?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(invalid_config(message));
    }
    Ok(path)
}

fn canonical_private_file(path: &Path, message: &'static str) -> Result<PathBuf, BoundaryError> {
    let path = fs::canonicalize(path).map_err(|_| invalid_config(message))?;
    require_safe_canonical_path(&path, message)?;
    let metadata = fs::metadata(&path).map_err(|_| invalid_config(message))?;
    let mode = metadata.permissions().mode();
    if !metadata.is_file() || mode & 0o077 != 0 || mode & 0o400 == 0 || mode & 0o6000 != 0 {
        return Err(invalid_config(message));
    }
    Ok(path)
}

fn canonical_read_file(path: &Path, message: &'static str) -> Result<PathBuf, BoundaryError> {
    let path = fs::canonicalize(path).map_err(|_| invalid_config(message))?;
    require_safe_canonical_path(&path, message)?;
    let metadata = fs::metadata(&path).map_err(|_| invalid_config(message))?;
    let mode = metadata.permissions().mode();
    if !metadata.is_file() || mode & 0o022 != 0 || mode & 0o400 == 0 {
        return Err(invalid_config(message));
    }
    Ok(path)
}

/// Symlinks are resolved before this check, so a link whose *target* contains
/// whitespace, control characters or quoting cannot be smuggled into the
/// generated SSH configuration or argv.
fn require_safe_canonical_path(path: &Path, message: &'static str) -> Result<(), BoundaryError> {
    if safe_local_path(path) {
        Ok(())
    } else {
        Err(invalid_config(message))
    }
}

fn validate_ssh_paths(paths: &VmLocalPaths) -> Result<(), BoundaryError> {
    for path in [&paths.ssh, &paths.identity_file, &paths.known_hosts] {
        require_safe_canonical_path(
            path,
            "Browser VM SSH inputs must not contain whitespace, control or quoting characters",
        )?;
    }
    Ok(())
}

/// One bounded non-interactive VM command. The script is a fixed constant;
/// arguments are validated inputs and are shell-quoted for the remote shell.
#[derive(Debug, Clone, Copy)]
pub struct VmCommandSpec<'a> {
    pub script: &'a str,
    pub args: &'a [String],
    pub stdin: Option<&'a [u8]>,
    pub timeout: Duration,
    pub stderr_limit: usize,
    /// Bound for stdout. File reads raise this above the default control size.
    pub stdout_limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmCommandOutput {
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
    pub cancelled: bool,
}

impl VmCommandOutput {
    pub fn succeeded(&self) -> bool {
        !self.timed_out && !self.cancelled && self.status == Some(0)
    }
}

/// Classify a completed VM command. `succeeded()` alone is not enough: a
/// dispatched command that timed out or was cancelled may have changed the
/// VM, so those outcomes become retryable `outcome_unknown`, while a
/// cancellation observed before dispatch remains a plain `cancelled`.
fn vm_command_outcome(
    output: VmCommandOutput,
    dispatched: bool,
    context: &'static str,
) -> Result<VmCommandOutput, BrowserFailure> {
    if output.cancelled {
        return if dispatched {
            Err(BrowserFailure::uncertain(format!(
                "{context} was cancelled after dispatch; the remote outcome is unknown"
            )))
        } else {
            Err(BrowserFailure::new(
                "cancelled",
                format!("{context} cancelled before dispatch"),
            ))
        };
    }
    if output.timed_out {
        return Err(BrowserFailure::uncertain(format!(
            "{context} timed out after dispatch; the remote outcome is unknown"
        )));
    }
    Ok(output)
}

/// Map a runner result onto a classified outcome. A failure reported before a
/// process exists is known and stays as-is; only failures that may have
/// happened after dispatch are conservatively mapped to `outcome_unknown`.
fn vm_command_result(
    result: Result<VmCommandOutput, BrowserFailure>,
    context: &'static str,
) -> Result<VmCommandOutput, BrowserFailure> {
    match result {
        Ok(output) => vm_command_outcome(output, true, context),
        Err(failure) if failure.code == "cancelled" => Err(failure),
        Err(failure) if !failure.uncertain => Err(failure),
        Err(_) => Err(BrowserFailure::uncertain(format!(
            "{context} could not be completed; the remote outcome is unknown"
        ))),
    }
}

/// Injectable remote command boundary used for identity and cleanup checks.
/// Production uses [`SshVmCommandRunner`]; tests use controlled doubles.
#[async_trait]
pub trait VmCommandRunner: Send + Sync {
    async fn run(
        &self,
        spec: VmCommandSpec<'_>,
        cancel: &dyn Cancellation,
    ) -> Result<VmCommandOutput, BrowserFailure>;
}

fn shell_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

fn remote_command(script: &str, args: &[String]) -> String {
    // The $0 label is deliberately distinct from the Run root prefix
    // (`saya-browser-vm-<hex>`). CLEANUP_SCRIPT uses it to exclude its own
    // shell and command-substitution children from the `/proc` scan without
    // also excluding the worker and Chromium, whose argv contains the Run
    // root.
    let mut command = format!("sh -c {} saya-browser-vm-cleanup", shell_quote(script));
    for argument in args {
        command.push(' ');
        command.push_str(&shell_quote(argument));
    }
    command
}

fn strict_ssh_config(
    settings: &BrowserVmSettings,
    paths: &VmLocalPaths,
) -> Result<String, BoundaryError> {
    let host = settings
        .host
        .as_deref()
        .and_then(normalized_loopback_host)
        .ok_or_else(|| invalid_config("Browser VM host is missing or not a loopback literal"))?;
    let user = settings
        .user
        .as_deref()
        .ok_or_else(|| invalid_config("Browser VM user is missing"))?;
    let port = settings
        .port
        .ok_or_else(|| invalid_config("Browser VM port is missing"))?;
    let profile = settings
        .profile
        .as_deref()
        .ok_or_else(|| invalid_config("Browser VM profile is missing"))?;
    if !safe_token(profile, 64) || !safe_token(user, 32) {
        return Err(invalid_config("Browser VM SSH identity is invalid"));
    }
    validate_ssh_paths(paths)?;
    Ok(format!(
        "# Saya dedicated browser VM. Generated per host channel; contains no secrets.\n\
         Host saya-browser-vm {host}\n\
         \x20 HostName {host}\n\
         \x20 User {user}\n\
         \x20 Port {port}\n\
         \x20 IdentityFile {identity}\n\
         \x20 IdentitiesOnly yes\n\
         \x20 BatchMode yes\n\
         \x20 PreferredAuthentications publickey\n\
         \x20 PasswordAuthentication no\n\
         \x20 KbdInteractiveAuthentication no\n\
         \x20 StrictHostKeyChecking yes\n\
         \x20 HostKeyAlias lima-colima-{profile}\n\
         \x20 UserKnownHostsFile {known_hosts}\n\
         \x20 GlobalKnownHostsFile /dev/null\n\
         \x20 UpdateHostKeys no\n\
         \x20 CheckHostIP yes\n\
         \x20 ForwardAgent no\n\
         \x20 ForwardX11 no\n\
         \x20 ForwardX11Trusted no\n\
         \x20 PermitLocalCommand no\n\
         \x20 ClearAllForwardings yes\n\
         \x20 ControlMaster no\n\
         \x20 ControlPath none\n\
         \x20 ProxyCommand none\n\
         \x20 ProxyJump none\n\
         \x20 ConnectTimeout {connect}\n\
         \x20 ServerAliveInterval 15\n\
         \x20 ServerAliveCountMax 3\n\
         \x20 RequestTTY no\n\
         \x20 LogLevel ERROR\n",
        identity = paths.identity_file.display(),
        known_hosts = paths.known_hosts.display(),
        connect = settings.connect_timeout_ms.div_ceil(1000).max(1),
    ))
}

/// One reverse streamlocal forward used as the VM's only browser egress.
/// sshd creates the socket on the VM; the SSH client connects its local side
/// to the per-Run broker on host loopback.
struct ReverseForward {
    remote_socket: String,
    local_port: u16,
}

fn ssh_argv(
    settings: &BrowserVmSettings,
    paths: &VmLocalPaths,
    config_path: &Path,
    script: &str,
    args: &[String],
    forward: Option<&ReverseForward>,
) -> Result<Vec<String>, BoundaryError> {
    let host = settings
        .host
        .as_deref()
        .and_then(normalized_loopback_host)
        .ok_or_else(|| invalid_config("Browser VM host is missing or not a loopback literal"))?;
    let user = settings
        .user
        .as_deref()
        .ok_or_else(|| invalid_config("Browser VM user is missing"))?;
    let port = settings
        .port
        .ok_or_else(|| invalid_config("Browser VM port is missing"))?;
    if !safe_token(user, 32) {
        return Err(invalid_config("Browser VM SSH identity is invalid"));
    }
    validate_ssh_paths(paths)?;
    let mut argv = vec![
        paths.ssh.display().to_string(),
        "-F".into(),
        config_path.display().to_string(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "IdentitiesOnly=yes".into(),
        "-o".into(),
        "StrictHostKeyChecking=yes".into(),
        "-o".into(),
        format!("UserKnownHostsFile={}", paths.known_hosts.display()),
        "-o".into(),
        "GlobalKnownHostsFile=/dev/null".into(),
        "-o".into(),
        "UpdateHostKeys=no".into(),
        "-o".into(),
        "ForwardAgent=no".into(),
        "-o".into(),
        "ClearAllForwardings=yes".into(),
        "-o".into(),
        "PermitLocalCommand=no".into(),
        "-o".into(),
        "ControlMaster=no".into(),
        "-o".into(),
        "ControlPath=none".into(),
        "-o".into(),
        "ProxyCommand=none".into(),
        "-o".into(),
        "ProxyJump=none".into(),
        "-o".into(),
        "PasswordAuthentication=no".into(),
        "-o".into(),
        "KbdInteractiveAuthentication=no".into(),
        "-o".into(),
        "PreferredAuthentications=publickey".into(),
        "-o".into(),
        format!(
            "ConnectTimeout={}",
            settings.connect_timeout_ms.div_ceil(1000).max(1)
        ),
        "-o".into(),
        "ServerAliveInterval=15".into(),
        "-o".into(),
        "ServerAliveCountMax=3".into(),
        "-o".into(),
        "LogLevel=ERROR".into(),
    ];
    if let Some(forward) = forward {
        // `ssh` uses the first obtained value for each option, so replace the
        // base ClearAllForwardings=yes instead of appending a later override.
        // A browser launch adds exactly one reverse streamlocal forward.
        for value in &mut argv {
            if value == "ClearAllForwardings=yes" {
                *value = "ClearAllForwardings=no".into();
            }
        }
        argv.extend([
            "-o".into(),
            "ExitOnForwardFailure=yes".into(),
            "-o".into(),
            "StreamLocalBindUnlink=yes".into(),
            "-R".into(),
            format!("{}:127.0.0.1:{}", forward.remote_socket, forward.local_port),
        ]);
    }
    argv.extend([
        "-p".into(),
        port.to_string(),
        "-l".into(),
        user.into(),
        "-i".into(),
        paths.identity_file.display().to_string(),
        host,
        remote_command(script, args),
    ]);
    Ok(argv)
}

fn private_directory(path: &Path) -> Result<(), BoundaryError> {
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    builder
        .create(path)
        .map_err(|_| invalid_config("Browser VM private directory cannot be created"))
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), BoundaryError> {
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = options
        .open(path)
        .map_err(|_| invalid_config("Browser VM private file cannot be created"))?;
    file.write_all(bytes)
        .map_err(|_| invalid_config("Browser VM private file cannot be written"))
}

/// One run-private host channel tree plus its prepared native environment.
struct VmHostEnvironment {
    environment: PreparedEnvironment,
    base: PathBuf,
    directories: BrowserSandboxDirectories,
}

impl VmHostEnvironment {
    fn create(
        settings: &BrowserVmSettings,
        proxy_port: Option<u16>,
    ) -> Result<Self, BoundaryError> {
        let paths = settings.preflight_local()?;
        settings.resolved_remote_paths()?;
        let root = fs::canonicalize(std::env::temp_dir())
            .map_err(|_| invalid_config("Browser VM host temp root is unavailable"))?;
        let base = root.join(format!(
            "saya-browser-vm-host-{}",
            uuid::Uuid::now_v7().simple()
        ));
        private_directory(&base)?;
        let directories = BrowserSandboxDirectories {
            profile: base.join("profile"),
            output: base.join("output"),
            tmp: base.join("tmp"),
            work: base.join("work"),
            config: base.join("config"),
        };
        let result = (|| {
            for path in [
                &directories.profile,
                &directories.output,
                &directories.tmp,
                &directories.work,
                &directories.config,
            ] {
                private_directory(path)?;
            }
            let policy = host_policy(settings, &base, &paths, proxy_port)?;
            let environment = execution::prepare(&policy)
                .map_err(|_| vm_unavailable("Browser VM host channel sandbox is unavailable"))?;
            Ok(Self {
                environment,
                base: base.clone(),
                directories,
            })
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&base);
        }
        result
    }
}

impl Drop for VmHostEnvironment {
    fn drop(&mut self) {
        let _ = self.environment.cleanup();
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// The host `ssh` process gets exactly what it needs: the executable, identity
/// file and known-hosts file read-only, a run-private writable tree, and
/// outbound TCP only to the VM's loopback SSH port.
fn host_policy(
    settings: &BrowserVmSettings,
    workspace: &Path,
    paths: &VmLocalPaths,
    proxy_port: Option<u16>,
) -> Result<SandboxPolicy, BoundaryError> {
    let port = settings
        .port
        .ok_or_else(|| invalid_config("Browser VM port is missing"))?;
    let host = settings
        .host
        .as_deref()
        .and_then(normalized_loopback_host)
        .and_then(|host| host.parse().ok())
        .ok_or_else(|| invalid_config("Browser VM host is not a loopback literal"))?;
    let mut policy = SandboxPolicy::workspace(workspace.to_path_buf());
    policy.read_roots.clear();
    policy.write_roots = vec![workspace.to_path_buf()];
    policy.read_files = vec![
        paths.ssh.clone(),
        paths.identity_file.clone(),
        paths.known_hosts.clone(),
    ];
    policy.protected_paths.clear();
    policy.network = NetworkPolicy::LoopbackPort {
        host,
        port,
        proxy_port,
    };
    policy.browser = None;
    Ok(policy)
}

/// Production runner. Every invocation prepares a fresh host environment and
/// supervises the SSH child through [`ManagedProcess`].
pub struct SshVmCommandRunner {
    settings: BrowserVmSettings,
    config: ExecConfig,
}

impl SshVmCommandRunner {
    pub fn new(settings: BrowserVmSettings) -> Result<Self, BoundaryError> {
        settings.preflight_local()?;
        Ok(Self {
            settings,
            config: ExecConfig::default(),
        })
    }

    pub fn with_exec_config(mut self, config: ExecConfig) -> Self {
        self.config = config;
        self
    }

    async fn run_once(
        &self,
        spec: VmCommandSpec<'_>,
        cancel: &dyn Cancellation,
    ) -> Result<VmCommandOutput, BrowserFailure> {
        if cancel.is_cancelled() {
            return Err(BrowserFailure::new(
                "cancelled",
                "Browser VM command cancelled before dispatch",
            ));
        }
        let paths = self
            .settings
            .preflight_local()
            .map_err(|failure| BrowserFailure::new(failure.0.code, failure.0.message))?;
        let host = VmHostEnvironment::create(&self.settings, None).map_err(|_| {
            vm_unavailable("Browser VM host channel cannot be prepared").into_browser_failure()
        })?;
        let config_path = host.directories.config.join("ssh_config");
        let config = strict_ssh_config(&self.settings, &paths)
            .map_err(|failure| BrowserFailure::new(failure.0.code, failure.0.message))?;
        write_private(&config_path, config.as_bytes())
            .map_err(|failure| BrowserFailure::new(failure.0.code, failure.0.message))?;
        let argv = ssh_argv(
            &self.settings,
            &paths,
            &config_path,
            spec.script,
            spec.args,
            None,
        )
        .map_err(|failure| BrowserFailure::new(failure.0.code, failure.0.message))?;
        let mut env = BTreeMap::new();
        env.insert(OsString::from("PATH"), OsString::from("/usr/bin:/bin"));
        env.insert(OsString::from("LANG"), OsString::from("C"));
        env.insert(OsString::from("NO_COLOR"), OsString::from("1"));
        env.insert(
            OsString::from("HOME"),
            host.directories.work.clone().into_os_string(),
        );
        let mut config = self.config.clone();
        config.execution = Some(host.environment.clone());
        let result = spawn_and_collect(
            &argv,
            &host.directories.work,
            env,
            &config,
            spec.timeout,
            spec.stderr_limit,
            spec.stdout_limit,
            spec.stdin,
            cancel,
        )
        .await;
        let _ = host.environment.wait_for_process_cleanup().await;
        drop(host);
        result
    }
}

#[async_trait]
impl VmCommandRunner for SshVmCommandRunner {
    async fn run(
        &self,
        spec: VmCommandSpec<'_>,
        cancel: &dyn Cancellation,
    ) -> Result<VmCommandOutput, BrowserFailure> {
        self.run_once(spec, cancel).await
    }
}

fn open_directory(path: &Path) -> Result<File, BrowserFailure> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    options.open(path).map_err(|_| {
        BrowserFailure::new("worker_spawn", "Browser VM work directory is unavailable")
    })
}

async fn drain_bounded<R>(mut reader: R, limit: usize) -> Vec<u8>
where
    R: AsyncRead + Unpin,
{
    let mut kept = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                if kept.len() < limit {
                    let remaining = limit - kept.len();
                    let take = remaining.min(count);
                    kept.extend_from_slice(&buffer[..take]);
                }
            }
        }
    }
    kept
}

#[allow(clippy::too_many_arguments)]
async fn spawn_and_collect(
    argv: &[String],
    cwd: &Path,
    env: BTreeMap<OsString, OsString>,
    config: &ExecConfig,
    timeout: Duration,
    stderr_limit: usize,
    stdout_limit: usize,
    input: Option<&[u8]>,
    cancel: &dyn Cancellation,
) -> Result<VmCommandOutput, BrowserFailure> {
    let plan = ExecPlan {
        argv: argv.to_vec(),
        cwd: cwd.to_path_buf(),
        timeout,
        environment_digest: worker::environment_digest(&env),
    };
    let cwd = {
        let file = open_directory(cwd)?;
        cap_std::fs::Dir::from_std_file(file)
    };
    let mut process =
        ManagedProcess::spawn(&plan, cwd, env, config, input.is_some()).map_err(|_| {
            BrowserFailure::new("worker_spawn", "Browser VM SSH process could not start")
        })?;
    let Some(stdout) = process.take_stdout() else {
        let _ = process.terminate().await;
        return Err(BrowserFailure::new(
            "worker_spawn",
            "Browser VM SSH stdout is unavailable",
        ));
    };
    let Some(stderr) = process.take_stderr() else {
        let _ = process.terminate().await;
        return Err(BrowserFailure::new(
            "worker_spawn",
            "Browser VM SSH stderr is unavailable",
        ));
    };
    let stdout_task = tokio::spawn(drain_bounded(stdout, stdout_limit));
    let stderr_task = tokio::spawn(drain_bounded(stderr, stderr_limit));
    if let Some(input) = input
        && let Some(mut stdin) = process.take_stdin()
        && (stdin.write_all(input).await.is_err() || stdin.shutdown().await.is_err())
    {
        let _ = process.terminate().await;
        return Err(BrowserFailure::new(
            "browser_vm_transport",
            "Browser VM command input could not be written",
        ));
    }
    let end = process.wait(cancel, timeout).await.map_err(|_| {
        BrowserFailure::new("browser_vm_transport", "Browser VM SSH command failed")
    })?;
    let stdout = stdout_task
        .await
        .map_err(|_| remote_failure("Browser VM stdout task failed"))?;
    let stderr = stderr_task
        .await
        .map_err(|_| remote_failure("Browser VM stderr task failed"))?;
    Ok(VmCommandOutput {
        status: end.status.code(),
        stdout,
        stderr,
        timed_out: end.timed_out,
        cancelled: end.cancelled,
    })
}

// ---------------------------------------------------------------------------
// Remote scripts. These are fixed strings; dynamic values arrive as arguments.
// ---------------------------------------------------------------------------

const PROBE_SCRIPT: &str = r#"set -eu
LC_ALL=C
export LC_ALL
for f in "$@"; do
  stat -c '%s|%a|%F' -- "$f"
  sha256sum -- "$f"
done
"#;

const CAT_SCRIPT: &str = "set -eu\ncat -- \"$1\"\n";

const NODE_VERSION_SCRIPT: &str = "set -eu\n\"$1\" --version\n";
const INSTALLATION_PROBE_SCRIPT: &str = "set -eu\ncd /\nexec env -i PATH=/usr/bin:/bin \"$1\" --no-addons - \"$2\" \"$3\" \"$4\" \"$5\"\n";
const INSTALLATION_PROBE: &str = include_str!("worker/installation_probe.js");

const PREPARE_RUN_SCRIPT: &str = r#"set -eu
d="$1"
[ ! -e "$d" ] || { echo "runroot=exists"; exit 3; }
umask 077
mkdir -- "$d"
chmod 700 "$d"
# LAUNCH_SCRIPT exports HOME, TMPDIR, all XDG base directories and the
# Playwright socket directory. Create every one of them here, under the same
# 0700 umask, so the worker never has to create state inside a directory with
# a weaker mode.
mkdir -- "$d/profile" "$d/output" "$d/tmp" "$d/work" "$d/config" \
  "$d/home" "$d/home/cache" "$d/home/data" "$d/home/state" "$d/home/runtime" \
  "$d/s"
chmod 700 "$d/profile" "$d/output" "$d/tmp" "$d/work" "$d/config" \
  "$d/home" "$d/home/cache" "$d/home/data" "$d/home/state" "$d/home/runtime" \
  "$d/s"
cat > "$d/config/config.json"
printf '%s' "$d"
"#;

const LAUNCH_SCRIPT: &str = r#"set -eu
runroot=$1; sock=$2; port=$3; pidfile=$4; shift 4
umask 077
cd "$runroot"
status_file="$runroot/worker.status"
stderr_file="$runroot/worker.stderr"
for tool in sudo unshare socat setpriv ip; do
  command -v "$tool" >/dev/null 2>&1 || { echo "network=$tool-missing" >&2; exit 41; }
done
[ -S "$sock" ] || { echo "network=proxy-socket-missing" >&2; exit 45; }
sudo -n true >/dev/null 2>&1 || { echo "network=sudo-unavailable" >&2; exit 46; }
uid=$(id -u)
gid=$(id -g)
# The worker and Chromium run inside a fresh network namespace created by
# root. Only loopback exists there; a socat listener bridges the SSH reverse
# streamlocal socket to 127.0.0.1:PORT. Privileges are then dropped back to the
# invoking user before the worker starts, and no host or VM-wide setting is
# changed. Any failure here is fatal: there is no default-network fallback.
sudo -n unshare -n -- /bin/sh -c '
  set -eu
  ip link set lo up 2>/dev/null || { echo "network=loopback-unavailable" >&2; exit 47; }
  if [ -n "$(ip -o route show 2>/dev/null)" ]; then
    echo "network=unexpected-route" >&2
    exit 48
  fi
  uid=$5
  gid=$6
  # Run the bridge as the invoking user, not root, so the same-user Run
  # cleanup can always terminate it. Only loopback setup needs root.
  setpriv --reuid="$uid" --regid="$gid" --init-groups -- \
    socat TCP-LISTEN:"$1",bind=127.0.0.1,reuseaddr,fork UNIX-CONNECT:"$2" \
    >/dev/null 2>&1 &
  sp=$!
  i=0
  listening=0
  while [ "$i" -lt 50 ]; do
    kill -0 "$sp" 2>/dev/null || break
    if command -v ss >/dev/null 2>&1 && ss -ltn 2>/dev/null | grep -q "127.0.0.1:$1 "; then
      listening=1
      break
    fi
    i=$((i + 1))
    sleep 0.1
  done
  if [ "$listening" = "0" ] && ! command -v ss >/dev/null 2>&1; then
    # Without ss, give socat a bounded head start; the worker will fail
    # closed through the proxy if it still is not listening.
    sleep 1
    listening=1
  fi
  kill -0 "$sp" 2>/dev/null || { echo "network=proxy-start-failed" >&2; exit 49; }
  [ "$listening" = "1" ] || { echo "network=proxy-not-listening" >&2; exit 50; }
  stop_proxy() { kill "$sp" 2>/dev/null || true; }
  trap stop_proxy TERM INT HUP
  runroot=$3; pidfile=$4; uid=$5; gid=$6; stderr=$7
  shift 7
  export HOME="$runroot/home"
  export TMPDIR="$runroot/tmp"
  export XDG_CONFIG_HOME="$runroot/config"
  export XDG_CACHE_HOME="$runroot/home/cache"
  export XDG_DATA_HOME="$runroot/home/data"
  export XDG_STATE_HOME="$runroot/home/state"
  export XDG_RUNTIME_DIR="$runroot/home/runtime"
  export PWTEST_SOCKETS_DIR="$runroot/s"
  export PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1
  export LANG=C
  export NO_COLOR=1
  export PATH=/usr/bin:/bin
  export SAYA_WORKER_STDERR="$stderr"
  status=0
  setpriv --reuid="$uid" --regid="$gid" --init-groups -- \
    setsid -w sh -c '"'"'printf "%s" "$$" > "$1"; shift; exec "$@" 2> "$SAYA_WORKER_STDERR"'"'"' \
      saya-browser-worker "$pidfile" "$@" || status=$?
  trap - TERM INT HUP
  stop_proxy
  exit "$status"
' sh "$port" "$sock" "$runroot" "$pidfile" "$uid" "$gid" "$stderr_file" "$@"
status=$?
printf '%s\n' "$status" > "$status_file"
exit "$status"
"#;

// These scripts run through POSIX `sh` on the VM (dash on Debian-family
// images). Keep `kill` invocations as `kill -TERM <pid>` / `kill -0 -<pgid>`:
// dash's `kill` builtin does not accept `--` as an option terminator and
// rejects `kill -0 -- <pid>` with "Illegal number", which would make a live
// worker look gone and let cleanup delete the Run root while it still runs.
const PID_SCRIPT: &str = r#"set -eu
p="$1"
node="$2"
cli="$3"
i=0
while [ "$i" -lt 10 ] && [ ! -s "$p" ]; do
  i=$((i + 1))
  sleep 1
done
[ -s "$p" ] || { echo "pid=missing"; exit 4; }
pid=$(cat -- "$p")
case "$pid" in
  ''|*[!0-9]*) echo "pid=invalid"; exit 5 ;;
esac
[ "$pid" -gt 1 ] || { echo "pid=invalid"; exit 5; }
kill -0 "$pid" 2>/dev/null || { echo "pid=gone"; exit 6; }
cmd=$(tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null || true)
case "$cmd" in
  *"$node"*) ;;
  *) echo "pid=wrong-process"; exit 7 ;;
esac
case "$cmd" in
  *"$cli"*) ;;
  *) echo "pid=wrong-worker"; exit 7 ;;
esac
printf 'pid=%s\n' "$pid"
printf 'cmdline=%s\n' "$cmd"
"#;

const CLEANUP_SCRIPT: &str = r#"set -eu
d="$1"
p="$2"
cli="$3"
expect="$4"
# The proc root is injectable so the script can be exercised against a
# controlled tree in regression tests. Production always uses /proc.
proc="${5:-/proc}"
if [ ! -e "$d" ]; then
  echo "worker=gone"
  echo "runroot=gone"
  exit 0
fi
scan() {
  [ "$2" -lt 8 ] || return 0
  for child in $(cat "$proc/$1/task/$1/children" 2>/dev/null || true); do
    echo "$child"
    scan "$child" $(( $2 + 1 ))
  done
}
# A process that still exists only as a zombie has already exited; it must
# count as gone so cleanup can finish after a clean worker shutdown.
live() {
  kill -0 "$1" 2>/dev/null || return 1
  state=$(awk '/^State:/{print $2}' "$proc/$1/status" 2>/dev/null || true)
  if [ -n "$state" ] && [ "$state" != "Z" ]; then
    return 0
  fi
  return 1
}
# The worker is a session/process-group leader; `kill -0 -<pid>` also succeeds
# for a group of unreaped zombies, so enumerate live members instead.
group_live() {
  for status in "$proc"/[0-9]*/status; do
    [ -r "$status" ] || continue
    pgid=$(awk '/^Pgid:/{print $2}' "$status" 2>/dev/null || true)
    [ "$pgid" = "$1" ] || continue
    state=$(awk '/^State:/{print $2}' "$status" 2>/dev/null || true)
    if [ -n "$state" ] && [ "$state" != "Z" ]; then
      return 0
    fi
  done
  return 1
}
any_live() {
  for target in $1; do
    live "$target" && return 0
  done
  return 1
}
# A bounded /proc scan for processes that reference the unique Run root. The
# PID file is only an observation; when it is missing, unwritten, stale or
# malformed the Run root must still be cleaned safely. This cleanup shell also
# carries the Run root in its argv, so its fixed label is excluded along with
# the shell and its parent.
related() {
  found=""
  for file in "$proc"/[0-9]*/cmdline; do
    [ -r "$file" ] || continue
    pid=${file%/cmdline}
    pid=${pid##*/}
    case "$pid" in
      ''|*[!0-9]*) continue ;;
    esac
    [ "$pid" -gt 1 ] || continue
    [ "$pid" = "$$" ] && continue
    [ "$pid" = "$PPID" ] && continue
    command=$(tr '\0' ' ' < "$file" 2>/dev/null || true)
    case "$command" in
      *"$d"*) ;;
      *) continue ;;
    esac
    case "$command" in
      *saya-browser-vm-cleanup*) continue ;;
    esac
    live "$pid" || continue
    found="$found $pid"
  done
  for pid in $found; do
    for child in $(scan "$pid" 0); do
      live "$child" && found="$found $child"
    done
  done
  printf '%s' "$found"
}
# Terminate a bounded set of PIDs, escalating to SIGKILL only after a bounded
# wait. Returns nonzero if any process is still live afterwards.
kill_set() {
  pids="$1"
  [ -n "$pids" ] || return 0
  for target in $pids; do
    kill -TERM "$target" 2>/dev/null || true
  done
  i=0
  while [ "$i" -lt 8 ] && any_live "$pids"; do
    i=$((i + 1))
    sleep 1
  done
  if any_live "$pids"; then
    for target in $pids; do
      kill -KILL "$target" 2>/dev/null || true
    done
    i=0
    while [ "$i" -lt 4 ] && any_live "$pids"; do
      i=$((i + 1))
      sleep 1
    done
  fi
  any_live "$pids" && return 1
  return 0
}
if [ -s "$p" ]; then
  pid=$(cat -- "$p" 2>/dev/null || true)
  case "$pid" in
    ''|*[!0-9]*) pid=0 ;;
  esac
  if [ "$pid" -gt 1 ] && kill -0 "$pid" 2>/dev/null && live "$pid"; then
    command=$(tr '\0' ' ' < "$proc/$pid/cmdline" 2>/dev/null || true)
    case "$command" in
      *"$cli"*) ;;
      *) echo "worker=mismatch"; exit 5 ;;
    esac
    pids=$( (echo "$pid"; scan "$pid" 0) | head -n 512 || true )
    # The proxy bridge (socat) and the sudo/unshare wrappers live outside the
    # worker's own process group but carry the unique Run root in their argv.
    # Union them so a Run can never leave a forwarding helper behind.
    extra=$(related)
    pids="$pids $extra"
    if ! kill_set "$pids"; then
      echo "worker=alive"; exit 6
    fi
    if group_live "$pid"; then
      kill -TERM "-$pid" 2>/dev/null || true
      i=0
      while [ "$i" -lt 8 ] && group_live "$pid"; do
        i=$((i + 1))
        sleep 1
      done
      if group_live "$pid"; then
        kill -KILL "-$pid" 2>/dev/null || true
        i=0
        while [ "$i" -lt 4 ] && group_live "$pid"; do
          i=$((i + 1))
          sleep 1
        done
      fi
      if group_live "$pid"; then
        echo "worker=alive"
        exit 6
      fi
    fi
    rm -rf -- "$d"
    [ ! -e "$d" ] || { echo "runroot=exists"; exit 7; }
    echo "worker=gone"
    echo "runroot=gone"
    exit 0
  fi
fi
# The PID file is absent, empty, stale or unusable. Scan for the unique Run
# root instead of reporting an unconfirmable worker: the observation cannot
# block a safe cleanup.
pids=$(related)
if ! kill_set "$pids"; then
  echo "worker=alive"
  exit 6
fi
rm -rf -- "$d"
[ ! -e "$d" ] || { echo "runroot=exists"; exit 7; }
if [ -n "$pids" ]; then
  echo "worker=gone"
else
  echo "worker=absent"
fi
echo "expect=$expect"
echo "runroot=gone"
"#;

const SANDBOX_SCRIPT: &str = r#"set -eu
root="$1"
scan() {
  [ "$2" -lt 8 ] || return 0
  for child in $(cat "/proc/$1/task/$1/children" 2>/dev/null || true); do
    echo "$child"
    scan "$child" $(( $2 + 1 ))
  done
}
i=0
while [ "$i" -lt 10 ]; do
  pids=$(scan "$root" 0 | head -n 512 || true)
  for pid in $pids; do
    cmd=$(tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null || true)
    case "$cmd" in
      *--type=renderer*)
        nnp=$(awk '/^NoNewPrivs:/{print $2}' "/proc/$pid/status" 2>/dev/null || true)
        sec=$(awk '/^Seccomp:/{print $2}' "/proc/$pid/status" 2>/dev/null || true)
        printf 'renderer=%s\n' "$pid"
        printf 'cmdline=%s\n' "$cmd"
        printf 'nonewprivs=%s\n' "$nnp"
        printf 'seccomp=%s\n' "$sec"
        exit 0
        ;;
    esac
  done
  i=$((i + 1))
  sleep 1
done
echo "renderer=none"
exit 4
"#;

// ---------------------------------------------------------------------------
// Identity verification.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ProbeRecord {
    size: u64,
    mode: u32,
    kind: String,
    digest: String,
}

fn parse_probe(output: &[u8], count: usize) -> Result<Vec<ProbeRecord>, BrowserFailure> {
    let text = std::str::from_utf8(output)
        .map_err(|_| identity_failure("Browser VM identity probe returned non-UTF-8 text"))?;
    let mut lines = text.lines();
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        let stat = lines
            .next()
            .ok_or_else(|| identity_failure("Browser VM identity probe is incomplete"))?;
        let mut fields = stat.split('|');
        let size = fields
            .next()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| identity_failure("Browser VM identity size is invalid"))?;
        let mode = fields
            .next()
            .and_then(|value| u32::from_str_radix(value, 8).ok())
            .ok_or_else(|| identity_failure("Browser VM identity mode is invalid"))?;
        let kind = fields
            .next()
            .ok_or_else(|| identity_failure("Browser VM identity type is missing"))?
            .to_owned();
        if fields.next().is_some() {
            return Err(identity_failure(
                "Browser VM identity probe line is malformed",
            ));
        }
        let hash_line = lines
            .next()
            .ok_or_else(|| identity_failure("Browser VM identity digest is missing"))?;
        let digest = hash_line
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_owned();
        if !valid_sha256(&digest) {
            return Err(identity_failure("Browser VM identity digest is invalid"));
        }
        records.push(ProbeRecord {
            size,
            mode,
            kind,
            digest,
        });
    }
    if lines.next().is_some() {
        return Err(identity_failure(
            "Browser VM identity probe returned extra data",
        ));
    }
    Ok(records)
}

fn is_regular(kind: &str) -> bool {
    kind == "regular file" || kind == "regular empty file"
}

fn check_probe(
    label: &'static str,
    record: &ProbeRecord,
    expected_size: u64,
    expected_digest: &str,
    executable: bool,
) -> Result<(), BrowserFailure> {
    if !is_regular(&record.kind) {
        return Err(identity_failure(format!(
            "Browser VM {label} is not a regular file"
        )));
    }
    if record.size != expected_size || record.size > MAX_IDENTITY_FILE_BYTES {
        return Err(identity_failure(format!(
            "Browser VM {label} size does not match its manifest"
        )));
    }
    if record.digest != expected_digest {
        return Err(identity_failure(format!(
            "Browser VM {label} digest does not match its manifest"
        )));
    }
    if record.mode & 0o022 != 0 || record.mode & 0o6000 != 0 || record.mode & 0o400 == 0 {
        return Err(identity_failure(format!(
            "Browser VM {label} permissions are unsafe"
        )));
    }
    if executable && record.mode & 0o111 == 0 {
        return Err(identity_failure(format!(
            "Browser VM {label} is not executable"
        )));
    }
    Ok(())
}

fn parse_manifest(bytes: &[u8]) -> Result<IdentityManifest, BrowserFailure> {
    let manifest: IdentityManifest = serde_json::from_slice(bytes)
        .map_err(|_| identity_failure("Browser VM identity manifest is invalid"))?;
    if manifest.schema_version != 2 {
        return Err(identity_failure(
            "Browser VM identity manifest schema version is unsupported",
        ));
    }
    manifest
        .installation_trees
        .validate()
        .map_err(|_| identity_failure("Browser VM installation tree identity is invalid"))?;
    for digest in [
        &manifest.node.sha256,
        &manifest.worker_cli_js.sha256,
        &manifest.mcp_package.sha256,
        &manifest.playwright_package.sha256,
        &manifest.playwright_core_package.sha256,
        &manifest.playwright_core_browsers_json.sha256,
        &manifest.chromium.sha256,
    ] {
        if !valid_sha256(digest) {
            return Err(identity_failure(
                "Browser VM identity manifest contains an invalid digest",
            ));
        }
    }
    Ok(manifest)
}

fn headless_shell_path(path: &str) -> bool {
    let path = Path::new(path);
    let name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
    let revision_directory = format!("chromium_headless_shell-{CHROMIUM_REVISION}");
    name == "chrome-headless-shell"
        && path
            .components()
            .any(|component| component.as_os_str().to_str() == Some(revision_directory.as_str()))
}

/// Verify the remote manifest and every named file through the controlled VM
/// channel. Digests, sizes, permissions, versions and the headless-shell path
/// are checked locally against the returned bytes; a successful remote exit
/// status alone is never accepted.
pub struct VmWorkerIdentityVerifier {
    settings: BrowserVmSettings,
    runner: Arc<dyn VmCommandRunner>,
}

impl VmWorkerIdentityVerifier {
    pub fn new(settings: BrowserVmSettings, runner: Arc<dyn VmCommandRunner>) -> Self {
        Self { settings, runner }
    }

    async fn read_remote_file(
        &self,
        path: &str,
        timeout: Duration,
        cancel: &dyn Cancellation,
    ) -> Result<Vec<u8>, BrowserFailure> {
        let args = [path.to_owned()];
        let output = vm_command_result(
            self.runner
                .run(
                    VmCommandSpec {
                        script: CAT_SCRIPT,
                        args: &args,
                        stdin: None,
                        timeout,
                        stderr_limit: 16 * 1024,
                        stdout_limit: 512 * 1024,
                    },
                    cancel,
                )
                .await,
            "Browser VM identity file read",
        )?;
        if !output.succeeded() {
            return Err(identity_failure("Browser VM identity file cannot be read"));
        }
        Ok(output.stdout)
    }
}

#[async_trait]
impl WorkerIdentityVerifier for VmWorkerIdentityVerifier {
    fn preflight(&self) -> Result<(), BoundaryError> {
        self.settings.preflight_local()?;
        self.settings.resolved_remote_paths()?;
        Ok(())
    }

    async fn verify(
        &self,
        cancel: &dyn Cancellation,
    ) -> Result<VerifiedWorkerIdentity, BrowserFailure> {
        if cancel.is_cancelled() {
            return Err(BrowserFailure::new(
                "cancelled",
                "Browser VM identity check cancelled before dispatch",
            ));
        }
        let remote = self
            .settings
            .resolved_remote_paths()
            .map_err(|_| identity_failure("Browser VM remote identity input is invalid"))?;
        let cli_js = format!(
            "{}/{}",
            remote.worker_root.trim_end_matches('/'),
            worker::CLI_JS_PATH
        );
        let mcp_package = format!(
            "{}/{}",
            remote.worker_root.trim_end_matches('/'),
            worker::MCP_PACKAGE_PATH
        );
        let playwright_package = format!(
            "{}/{}",
            remote.worker_root.trim_end_matches('/'),
            worker::PLAYWRIGHT_PACKAGE_PATH
        );
        let playwright_core = format!(
            "{}/{}",
            remote.worker_root.trim_end_matches('/'),
            worker::PLAYWRIGHT_CORE_PACKAGE_PATH
        );
        let browsers_json = format!(
            "{}/{}",
            remote.worker_root.trim_end_matches('/'),
            worker::BROWSERS_JSON_PATH
        );
        if !headless_shell_path(&remote.chromium) {
            return Err(identity_failure(
                "Browser VM Chromium path is not the pinned headless-shell binary",
            ));
        }

        let files = [
            remote.identity_manifest.clone(),
            remote.node.clone(),
            cli_js.clone(),
            mcp_package.clone(),
            playwright_package.clone(),
            playwright_core.clone(),
            browsers_json.clone(),
            remote.chromium.clone(),
        ];
        let timeout = Duration::from_millis(self.settings.request_timeout_ms);
        let probe = vm_command_result(
            self.runner
                .run(
                    VmCommandSpec {
                        script: PROBE_SCRIPT,
                        args: &files,
                        stdin: None,
                        timeout,
                        stderr_limit: 16 * 1024,
                        stdout_limit: 512 * 1024,
                    },
                    cancel,
                )
                .await,
            "Browser VM identity probe",
        )?;
        if !probe.succeeded() {
            return Err(identity_failure(
                "Browser VM identity probe could not inspect the pinned files",
            ));
        }
        let records = parse_probe(&probe.stdout, files.len())?;

        let manifest_bytes = self
            .read_remote_file(&remote.identity_manifest, timeout, cancel)
            .await?;
        if manifest_bytes.len() as u64 > MAX_MANIFEST_BYTES
            || manifest_bytes.len() as u64 != records[0].size
            || sha256_hex(&manifest_bytes) != remote.identity_sha256
            || records[0].digest != remote.identity_sha256
        {
            return Err(identity_failure(
                "Browser VM identity manifest digest does not match the host input",
            ));
        }
        if !is_regular(&records[0].kind) {
            return Err(identity_failure(
                "Browser VM identity manifest is not a regular file",
            ));
        }
        if records[0].mode & 0o022 != 0
            || records[0].mode & 0o6000 != 0
            || records[0].mode & 0o400 == 0
        {
            return Err(identity_failure(
                "Browser VM identity manifest permissions are unsafe",
            ));
        }
        let manifest = parse_manifest(&manifest_bytes)?;

        check_probe(
            "node",
            &records[1],
            manifest.node.size,
            &manifest.node.sha256,
            true,
        )?;
        check_probe(
            "worker CLI",
            &records[2],
            manifest.worker_cli_js.size,
            &manifest.worker_cli_js.sha256,
            false,
        )?;
        check_probe(
            "MCP package",
            &records[3],
            manifest.mcp_package.size,
            &manifest.mcp_package.sha256,
            false,
        )?;
        check_probe(
            "Playwright package",
            &records[4],
            manifest.playwright_package.size,
            &manifest.playwright_package.sha256,
            false,
        )?;
        check_probe(
            "Playwright core package",
            &records[5],
            manifest.playwright_core_package.size,
            &manifest.playwright_core_package.sha256,
            false,
        )?;
        check_probe(
            "browsers.json",
            &records[6],
            manifest.playwright_core_browsers_json.size,
            &manifest.playwright_core_browsers_json.sha256,
            false,
        )?;
        check_probe(
            "Chromium",
            &records[7],
            manifest.chromium.size,
            &manifest.chromium.sha256,
            true,
        )?;

        // Inspect the returned bytes instead of trusting the remote exit code.
        for (path, expected) in [
            (&mcp_package, &manifest.mcp_package),
            (&playwright_package, &manifest.playwright_package),
            (&playwright_core, &manifest.playwright_core_package),
        ] {
            let bytes = self.read_remote_file(path, timeout, cancel).await?;
            if bytes.len() as u64 != expected.size || sha256_hex(&bytes) != expected.sha256 {
                return Err(identity_failure(
                    "Browser VM package bytes changed during verification",
                ));
            }
            let package = parse_package(
                &bytes,
                if path == &mcp_package {
                    "@playwright/mcp"
                } else if path == &playwright_package {
                    "playwright"
                } else {
                    "playwright-core"
                },
            )
            .map_err(|_| identity_failure("Browser VM package metadata is invalid"))?;
            if package.version != expected.version {
                return Err(identity_failure(
                    "Browser VM package version does not match its manifest",
                ));
            }
        }
        if manifest.mcp_package.version != PLAYWRIGHT_MCP_VERSION {
            return Err(identity_failure(
                "Browser VM @playwright/mcp version does not match the pinned version",
            ));
        }
        if manifest.playwright_package.version != PLAYWRIGHT_VERSION
            || manifest.playwright_core_package.version != PLAYWRIGHT_VERSION
        {
            return Err(identity_failure(
                "Browser VM Playwright version does not match the pinned version",
            ));
        }

        let browsers_bytes = self
            .read_remote_file(&browsers_json, timeout, cancel)
            .await?;
        if browsers_bytes.len() as u64 != manifest.playwright_core_browsers_json.size
            || sha256_hex(&browsers_bytes) != manifest.playwright_core_browsers_json.sha256
        {
            return Err(identity_failure(
                "Browser VM browsers.json changed during verification",
            ));
        }
        let browsers = parse_browsers(&browsers_bytes)
            .map_err(|_| identity_failure("Browser VM browsers.json is invalid"))?;
        let record = browsers
            .browsers
            .iter()
            .find(|record| record.name == "chromium")
            .ok_or_else(|| {
                identity_failure("Browser VM browsers.json does not declare Chromium")
            })?;
        let revision = record
            .revision
            .parse::<u32>()
            .map_err(|_| identity_failure("Browser VM Chromium revision is invalid"))?;
        if revision != CHROMIUM_REVISION
            || record.browser_version.as_deref() != Some(CHROMIUM_VERSION)
            || manifest.chromium.revision != CHROMIUM_REVISION
            || manifest.chromium.version != CHROMIUM_VERSION
        {
            return Err(identity_failure(
                "Browser VM Chromium version does not match the pinned version",
            ));
        }

        let parent = |path: &str| {
            Path::new(path)
                .parent()
                .and_then(Path::to_str)
                .map(str::to_owned)
                .ok_or_else(|| identity_failure("Browser VM installation root is missing"))
        };
        let tree_args = [
            remote.node.clone(),
            parent(&remote.node)?,
            remote.worker_root.clone(),
            parent(&remote.chromium)?,
            self.settings.connect_timeout_ms.to_string(),
        ];
        let tree_probe = vm_command_result(
            self.runner
                .run(
                    VmCommandSpec {
                        script: INSTALLATION_PROBE_SCRIPT,
                        args: &tree_args,
                        stdin: Some(INSTALLATION_PROBE.as_bytes()),
                        timeout,
                        stderr_limit: 16 * 1024,
                        stdout_limit: 4096,
                    },
                    cancel,
                )
                .await,
            "Browser VM installation tree probe",
        )?;
        if !tree_probe.succeeded()
            || !tree_probe.stderr.is_empty()
            || tree_probe.stdout.len() > 4096
        {
            return Err(identity_failure(
                "Browser VM installation tree probe failed",
            ));
        }
        let trees: worker::InstallationTrees = serde_json::from_slice(&tree_probe.stdout)
            .map_err(|_| identity_failure("Browser VM installation tree probe is invalid"))?;
        trees
            .validate()
            .map_err(|_| identity_failure("Browser VM installation tree probe is invalid"))?;
        if trees != manifest.installation_trees {
            return Err(identity_failure(
                "Browser VM installation tree differs from its manifest",
            ));
        }

        let version_args = [remote.node.clone()];
        let version = vm_command_result(
            self.runner
                .run(
                    VmCommandSpec {
                        script: NODE_VERSION_SCRIPT,
                        args: &version_args,
                        stdin: None,
                        timeout,
                        stderr_limit: 16 * 1024,
                        stdout_limit: 512 * 1024,
                    },
                    cancel,
                )
                .await,
            "Browser VM Node version probe",
        )?;
        if !version.succeeded() {
            return Err(identity_failure("Browser VM Node version cannot be read"));
        }
        let actual = std::str::from_utf8(&version.stdout)
            .map_err(|_| identity_failure("Browser VM Node version is not UTF-8"))?
            .trim();
        if actual.is_empty() || actual != manifest.node.version {
            return Err(identity_failure(
                "Browser VM Node version does not match its manifest",
            ));
        }

        Ok(VerifiedWorkerIdentity {
            node: PathBuf::from(remote.node),
            worker_root: PathBuf::from(remote.worker_root),
            cli_js: PathBuf::from(cli_js),
            chromium: PathBuf::from(remote.chromium),
            manifest_sha256: remote.identity_sha256,
        })
    }
}

// ---------------------------------------------------------------------------
// Launch planning and execution.
// ---------------------------------------------------------------------------

/// Plans a VM request. The remote Run root is chosen by the launcher at each
/// lifecycle, so the request carries only shared limits and the run-private
/// host directory layout.
pub struct VmWorkerLaunchPlanner {
    settings: BrowserVmSettings,
}

impl VmWorkerLaunchPlanner {
    pub fn new(settings: BrowserVmSettings) -> Self {
        Self { settings }
    }
}

impl WorkerLaunchPlanner for VmWorkerLaunchPlanner {
    fn build(
        &self,
        identity: &VerifiedWorkerIdentity,
        directories: &BrowserSandboxDirectories,
    ) -> Result<WorkerLaunchRequest, BoundaryError> {
        self.settings.validate()?;
        Ok(WorkerLaunchRequest {
            // The VM launcher builds its own SSH argv after creating the
            // remote Run root. These fields remain empty for that backend.
            argv: Vec::new(),
            cwd: directories.work.clone(),
            env: BTreeMap::new(),
            timeout: Duration::from_millis(self.settings.request_timeout_ms.saturating_add(30_000)),
            stderr_limit: self.settings.stderr_limit_bytes,
            mcp_limits: McpLimits {
                connect_timeout: Duration::from_millis(self.settings.connect_timeout_ms),
                request_timeout: Duration::from_millis(self.settings.request_timeout_ms),
                max_wire_bytes: 4 * 1024 * 1024 + 1024 * 1024,
                max_pages: 32,
                max_tools: 256,
                max_catalog_bytes: 4 * 1024 * 1024,
                max_result_bytes: 4 * 1024 * 1024,
                max_content_blocks: 256,
            },
            directories: directories.clone(),
            execution: None,
            identity: Some(identity.clone()),
        })
    }
}

fn worker_config_json(
    settings: &BrowserVmSettings,
    run_root: &str,
    proxy_url: &str,
) -> Result<Vec<u8>, BoundaryError> {
    let chromium = settings
        .chromium
        .as_deref()
        .ok_or_else(|| invalid_config("Browser VM Chromium path is missing"))?;
    let config = serde_json::json!({
        "browser": {
            "browserName": "chromium",
            "isolated": true,
            "launchOptions": {
                "headless": true,
                "chromiumSandbox": true,
                "executablePath": chromium,
                // Playwright passes this to Chromium's --proxy-server. The
                // address is the loopback listener inside the Run's private
                // network namespace; no other egress exists there.
                "proxy": {"server": proxy_url},
                // No implicit loopback bypass, no QUIC/HTTP3, no direct or
                // non-proxied WebRTC UDP, and no local name resolution. The
                // broker performs all DNS work for allowed origins.
                "args": [
                    format!("--proxy-server={proxy_url}"),
                    "--proxy-bypass-list=<-loopback>".to_owned(),
                    "--disable-quic".to_owned(),
                    "--force-webrtc-ip-handling-policy=disable_non_proxied_udp".to_owned(),
                    "--webrtc-ip-handling-policy=disable_non_proxied_udp".to_owned(),
                    "--host-resolver-rules=EXCLUDE 127.0.0.1, MAP * ~NOTFOUND".to_owned(),
                    "--disable-features=DnsOverHttps".to_owned(),
                    "--no-pings".to_owned(),
                    "--disable-background-networking".to_owned(),
                ],
            },
        },
        "outputDir": format!("{run_root}/output"),
        "imageResponses": "omit",
        "webmcp": false,
    });
    serde_json::to_vec_pretty(&config)
        .map_err(|_| invalid_config("Browser VM worker config cannot be encoded"))
}

/// Remote cleanup through the controlled channel. Used by the connection and
/// by launch-failure paths, so remote removal is never inferred from a local
/// SSH disconnect.
#[async_trait]
pub trait VmRemoteControl: Send + Sync {
    async fn terminate(&self, expect_worker: bool) -> Result<(), BrowserFailure>;
    /// Copy one host-staged upload into the Run's private remote staging area.
    async fn put_upload(
        &self,
        _filename: &str,
        _bytes: &[u8],
        _cancel: &dyn Cancellation,
    ) -> Result<String, BrowserFailure> {
        Err(BrowserFailure::unsupported("VM upload staging"))
    }
    /// Remove one Run-private staging upload.
    async fn remove_upload(
        &self,
        _path: &str,
        _cancel: &dyn Cancellation,
    ) -> Result<(), BrowserFailure> {
        Ok(())
    }
    /// Read one worker-produced download under the Run's output directory.
    async fn read_download(
        &self,
        _path: &str,
        _max_bytes: usize,
        _cancel: &dyn Cancellation,
    ) -> Result<Vec<u8>, BrowserFailure> {
        Err(BrowserFailure::unsupported("VM download read"))
    }
    /// List regular files in the Run's remote output directory.
    async fn list_downloads(
        &self,
        _cancel: &dyn Cancellation,
    ) -> Result<BTreeSet<String>, BrowserFailure> {
        Err(BrowserFailure::unsupported("VM download listing"))
    }
}

const UPLOAD_SCRIPT: &str = r#"set -eu
root=$1
name=$2
case "$name" in
  ''|.*|-*|*/*|*\\*) exit 2 ;;
esac
mkdir -p "$root/uploads"
chmod 700 "$root/uploads"
umask 077
tmp="$root/uploads/.upload.$$.tmp"
cleanup() { rm -f -- "$tmp"; }
trap cleanup EXIT HUP INT TERM
cat > "$tmp"
chmod 600 "$tmp"
[ ! -e "$root/uploads/$name" ] || exit 3
mv -f -- "$tmp" "$root/uploads/$name"
trap - EXIT HUP INT TERM
printf '%s\n' "$root/uploads/$name"
"#;

const REMOVE_UPLOAD_SCRIPT: &str = r#"set -eu
root=$1
path=$2
case "$path" in
  "$root"/uploads/*) rest=${path#"$root"/uploads/} ;;
  *) exit 2 ;;
esac
case "$rest" in
  ''|.*|*/*|*\\*) exit 2 ;;
esac
rm -f -- "$path"
"#;

const DOWNLOAD_READ_SCRIPT: &str = r#"set -eu
root=$1
path=$2
max=$3
case "$path" in
  "$root"/output/*) ;;
  *) exit 2 ;;
esac
case "$path" in
  */../*|*/..|../*|..|*/./*|*/.) exit 2 ;;
esac
[ -f "$path" ] && [ ! -L "$path" ] || exit 3
size=$(stat -c%s "$path")
[ "$size" -ge 1 ] && [ "$size" -le "$max" ] || exit 4
cat "$path"
"#;

const LIST_DOWNLOADS_SCRIPT: &str = r#"set -eu
root=$1
dir="$root/output"
case "$dir" in
  *..*) exit 2 ;;
esac
[ -d "$dir" ] || exit 0
for f in "$dir"/*; do
  [ -e "$f" ] || continue
  [ -f "$f" ] && [ ! -L "$f" ] || continue
  b=${f##*/}
  case "$b" in
    .*) continue ;;
  esac
  printf '%s\n' "$b"
done
"#;

struct RunnerRemoteControl {
    settings: BrowserVmSettings,
    runner: Arc<dyn VmCommandRunner>,
    run_root: String,
    pid_file: String,
    cli_js: String,
}

#[async_trait]
impl VmRemoteControl for RunnerRemoteControl {
    async fn terminate(&self, expect_worker: bool) -> Result<(), BrowserFailure> {
        let args = [
            self.run_root.clone(),
            self.pid_file.clone(),
            self.cli_js.clone(),
            if expect_worker {
                "1".into()
            } else {
                "0".into()
            },
        ];
        let timeout =
            Duration::from_millis(self.settings.cleanup_timeout_ms.saturating_add(15_000));
        let output = vm_command_result(
            self.runner
                .run(
                    VmCommandSpec {
                        script: CLEANUP_SCRIPT,
                        args: &args,
                        stdin: None,
                        timeout,
                        stderr_limit: 16 * 1024,
                        stdout_limit: 512 * 1024,
                    },
                    &NeverCancel,
                )
                .await,
            "Browser VM worker cleanup",
        )
        .map_err(|_| remote_cleanup_failure("Browser VM worker cleanup could not be confirmed"))?;
        let text = String::from_utf8_lossy(&output.stdout);
        let confirmed_worker = text.contains("worker=gone") || text.contains("worker=absent");
        let confirmed_root = text.contains("runroot=gone");
        if !output.succeeded() || !confirmed_worker || !confirmed_root {
            return Err(remote_cleanup_failure(
                "Browser VM worker cleanup could not be confirmed",
            ));
        }
        Ok(())
    }

    async fn put_upload(
        &self,
        filename: &str,
        bytes: &[u8],
        cancel: &dyn Cancellation,
    ) -> Result<String, BrowserFailure> {
        let name = filename
            .rsplit(['/', '\\'])
            .next()
            .filter(|name| {
                !name.is_empty()
                    && *name != "."
                    && *name != ".."
                    && !name.starts_with('.')
                    && name.len() <= 128
                    && !name.chars().any(char::is_control)
            })
            .ok_or_else(|| {
                BrowserFailure::new("file_name_invalid", "Browser VM upload name is unsafe")
            })?;
        let args = [self.run_root.clone(), name.to_owned()];
        let timeout = Duration::from_millis(self.settings.request_timeout_ms.max(15_000));
        let output = vm_command_result(
            self.runner
                .run(
                    VmCommandSpec {
                        script: UPLOAD_SCRIPT,
                        args: &args,
                        stdin: Some(bytes),
                        timeout,
                        stderr_limit: 16 * 1024,
                        stdout_limit: 16 * 1024,
                    },
                    cancel,
                )
                .await,
            "Browser VM upload staging",
        )?;
        if !output.succeeded() {
            return Err(remote_failure(
                "Browser VM upload staging could not be confirmed",
            ));
        }
        let path = String::from_utf8(output.stdout)
            .map_err(|_| remote_failure("Browser VM upload path is invalid"))?
            .trim()
            .to_owned();
        if !path.starts_with(&format!("{}/uploads/", self.run_root)) || path.len() > 4096 {
            return Err(remote_failure(
                "Browser VM upload staging could not be confirmed",
            ));
        }
        Ok(path)
    }

    async fn remove_upload(
        &self,
        path: &str,
        cancel: &dyn Cancellation,
    ) -> Result<(), BrowserFailure> {
        let expected = format!("{}/uploads/", self.run_root);
        let staged = path.strip_prefix(&expected).unwrap_or_default();
        if staged.is_empty()
            || staged.starts_with('.')
            || staged.contains(['/', '\\'])
            || path.len() > 4096
        {
            return Err(BrowserFailure::new(
                "file_path_denied",
                "Browser VM upload staging path is outside the Run staging directory",
            ));
        }
        let args = [self.run_root.clone(), path.to_owned()];
        let timeout = Duration::from_millis(self.settings.cleanup_timeout_ms.max(15_000));
        let output = vm_command_result(
            self.runner
                .run(
                    VmCommandSpec {
                        script: REMOVE_UPLOAD_SCRIPT,
                        args: &args,
                        stdin: None,
                        timeout,
                        stderr_limit: 16 * 1024,
                        stdout_limit: 16 * 1024,
                    },
                    cancel,
                )
                .await,
            "Browser VM upload staging cleanup",
        )?;
        if !output.succeeded() {
            return Err(BrowserFailure::new(
                "file_broker",
                "Browser VM upload staging could not be removed",
            ));
        }
        Ok(())
    }

    async fn read_download(
        &self,
        path: &str,
        max_bytes: usize,
        cancel: &dyn Cancellation,
    ) -> Result<Vec<u8>, BrowserFailure> {
        let expected = format!("{}/output/", self.run_root);
        // Playwright reports a path relative to the worker cwd in some
        // versions. Resolve it against the Run root, but never accept `..`
        // components or a path outside the Run output directory.
        let resolved = if path.starts_with('/') {
            path.to_owned()
        } else if path.contains(['/', '\\']) {
            format!("{}/{}", self.run_root, path)
        } else {
            format!("{}/output/{}", self.run_root, path)
        };
        let relative = resolved.strip_prefix(&expected).unwrap_or_default();
        // Reject traversal components, not a `..` substring inside a legal
        // basename such as `invoice..txt`.
        let components_ok = !relative.is_empty()
            && relative
                .split(['/', '\\'])
                .all(|component| !component.is_empty() && component != "." && component != "..");
        if !components_ok || resolved.len() > 4096 || resolved.chars().any(char::is_control) {
            return Err(BrowserFailure::new(
                "file_path_denied",
                "Browser VM download path is outside the Run output directory",
            ));
        }
        let args = [self.run_root.clone(), resolved, max_bytes.to_string()];
        let timeout = Duration::from_millis(self.settings.request_timeout_ms.max(15_000));
        let output = vm_command_result(
            self.runner
                .run(
                    VmCommandSpec {
                        script: DOWNLOAD_READ_SCRIPT,
                        args: &args,
                        stdin: None,
                        timeout,
                        stderr_limit: 16 * 1024,
                        stdout_limit: max_bytes.saturating_add(1024),
                    },
                    cancel,
                )
                .await,
            "Browser VM download read",
        )?;
        if !output.succeeded() || output.stdout.is_empty() || output.stdout.len() > max_bytes {
            return Err(remote_failure(
                "Browser VM download file could not be read within its limit",
            ));
        }
        Ok(output.stdout)
    }

    async fn list_downloads(
        &self,
        cancel: &dyn Cancellation,
    ) -> Result<BTreeSet<String>, BrowserFailure> {
        let args = [self.run_root.clone()];
        let timeout = Duration::from_millis(self.settings.request_timeout_ms.max(15_000));
        let output = vm_command_result(
            self.runner
                .run(
                    VmCommandSpec {
                        script: LIST_DOWNLOADS_SCRIPT,
                        args: &args,
                        stdin: None,
                        timeout,
                        stderr_limit: 16 * 1024,
                        stdout_limit: 256 * 1024,
                    },
                    cancel,
                )
                .await,
            "Browser VM download listing",
        )?;
        if !output.succeeded() {
            return Err(remote_failure(
                "Browser VM download directory could not be listed",
            ));
        }
        let text = std::str::from_utf8(&output.stdout)
            .map_err(|_| remote_failure("Browser VM download listing is invalid"))?;
        let mut names = BTreeSet::new();
        for line in text.lines() {
            let name = line.trim_end_matches('\r');
            if name.is_empty()
                || name.len() > 255
                || name.contains('/')
                || name.contains('\0')
                || name.chars().any(char::is_control)
                || name.starts_with('.')
            {
                return Err(remote_failure(
                    "Browser VM download listing contains an unsafe name",
                ));
            }
            if name.ends_with(".crdownload")
                || name.ends_with(".part")
                || name.ends_with(".tmp")
                || (name.starts_with("page-")
                    && (name.ends_with(".yml") || name.ends_with(".yaml")))
            {
                continue;
            }
            names.insert(name.to_owned());
        }
        Ok(names)
    }
}
struct NeverCancel;
impl Cancellation for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}

struct VmSpawned {
    process: ManagedProcess,
    stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
    stderr_tail: Arc<Mutex<VecDeque<u8>>>,
    stderr_task: Option<tokio::task::JoinHandle<()>>,
}

/// The launch script emits only these fixed `network=` failure families for a
/// missing or unusable private-network prerequisite. Anything else stays a
/// transport failure so a generic worker crash is never mislabeled as a
/// network policy problem, and there is never a default-VM-egress fallback.
const NETWORK_FAILURE_PREFIXES: &[&[u8]] = &[
    b"network=sudo-",
    b"network=unshare-",
    b"network=socat-",
    b"network=setpriv-",
    b"network=ip-",
    b"network=loopback-",
    b"network=unexpected-route",
    b"network=proxy-",
    // With ExitOnForwardFailure=yes, an sshd that refuses reverse
    // streamlocal forwarding terminates the session before LAUNCH_SCRIPT
    // runs, so the client's own fixed diagnostic is the only sentinel.
    b"remote port forwarding failed",
];

fn classify_launch_failure(cancel: &dyn Cancellation, stderr: &[u8]) -> BrowserFailure {
    if cancel.is_cancelled() {
        return BrowserFailure::new("cancelled", "Browser VM worker initialization cancelled");
    }
    let network_failed = NETWORK_FAILURE_PREFIXES
        .iter()
        .any(|prefix| stderr.windows(prefix.len()).any(|window| window == *prefix));
    if network_failed {
        BrowserFailure::new(
            "browser_network_unavailable",
            "Browser VM network isolation could not be established",
        )
    } else {
        remote_failure("Browser VM worker initialization failed")
    }
}

/// Await the bounded stderr drain before classifying a startup failure. The
/// outer process timeout may have aborted the read loop, so this waits only
/// for a short bounded moment and then reads whatever the shared deque holds.
async fn drain_stderr_bounded(
    stderr_tail: &Arc<Mutex<VecDeque<u8>>>,
    stderr_task: Option<tokio::task::JoinHandle<()>>,
) -> Vec<u8> {
    if let Some(task) = stderr_task {
        let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
    }
    stderr_tail
        .lock()
        .map(|tail| tail.iter().copied().collect())
        .unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
fn spawn_remote_worker(
    argv: &[String],
    cwd: &Path,
    env: BTreeMap<OsString, OsString>,
    environment: PreparedEnvironment,
    config: &ExecConfig,
    timeout: Duration,
    stderr_limit: usize,
) -> Result<VmSpawned, BrowserFailure> {
    let plan = ExecPlan {
        argv: argv.to_vec(),
        cwd: cwd.to_path_buf(),
        timeout,
        environment_digest: worker::environment_digest(&env),
    };
    let cwd = {
        let file = open_directory(cwd)?;
        cap_std::fs::Dir::from_std_file(file)
    };
    let mut config = config.clone();
    config.execution = Some(environment);
    let mut process = ManagedProcess::spawn(&plan, cwd, env, &config, true).map_err(|_| {
        BrowserFailure::new("worker_spawn", "Browser VM SSH worker could not start")
    })?;
    let Some(stdin) = process.take_stdin() else {
        drop(process);
        return Err(BrowserFailure::new(
            "worker_spawn",
            "Browser VM SSH stdin is unavailable",
        ));
    };
    let Some(stdout) = process.take_stdout() else {
        drop(process);
        return Err(BrowserFailure::new(
            "worker_spawn",
            "Browser VM SSH stdout is unavailable",
        ));
    };
    let stderr_tail = Arc::new(Mutex::new(VecDeque::with_capacity(stderr_limit)));
    let stderr_task = if let Some(mut stderr) = process.take_stderr() {
        let tail = stderr_tail.clone();
        let keep = stderr_limit;
        Some(tokio::spawn(async move {
            let mut buffer = [0u8; 8192];
            loop {
                match stderr.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        if let Ok(mut tail) = tail.lock() {
                            for byte in &buffer[..count] {
                                if tail.len() == keep {
                                    tail.pop_front();
                                }
                                tail.push_back(*byte);
                            }
                        }
                    }
                }
            }
        }))
    } else {
        None
    };
    Ok(VmSpawned {
        process,
        stdin,
        stdout,
        stderr_tail,
        stderr_task,
    })
}

/// Shared confirmation state for the remote Run root. Exactly one cleanup
/// path may claim it; a successful termination is never repeated, and a
/// failed one stays pending for a retry.
struct ConnectionCleanup {
    control: Arc<dyn VmRemoteControl>,
    done: std::sync::atomic::AtomicBool,
    lock: AsyncMutex<()>,
}

impl ConnectionCleanup {
    fn new(control: Arc<dyn VmRemoteControl>) -> Self {
        Self {
            control,
            done: std::sync::atomic::AtomicBool::new(false),
            lock: AsyncMutex::new(()),
        }
    }

    async fn terminate(&self, expect_worker: bool) -> Result<(), BrowserFailure> {
        let _guard = self.lock.lock().await;
        if self.done.load(Ordering::Acquire) {
            return Ok(());
        }
        let result = self.control.terminate(expect_worker).await;
        if result.is_ok() {
            self.done.store(true, Ordering::Release);
        }
        result
    }

    fn pending(&self) -> bool {
        !self.done.load(Ordering::Acquire)
    }
}

/// Schedule the best-effort Drop cleanup. A Tokio runtime may be absent when
/// the provider is dropped during host shutdown; the remote outcome is then
/// explicitly logged as unknown rather than treated as success.
fn schedule_drop_cleanup(cleanup: &Arc<ConnectionCleanup>) -> bool {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        tracing::warn!(
            target: "saya::browser",
            event = "browser.vm.cleanup_unknown",
            reason = "no Tokio runtime for Drop cleanup; the remote Run root may remain"
        );
        return false;
    };
    let cleanup = cleanup.clone();
    handle.spawn(async move {
        if let Err(failure) = cleanup.terminate(true).await {
            tracing::warn!(
                target: "saya::browser",
                event = "browser.vm.cleanup_unknown",
                code = %failure.code,
                "Browser VM Drop cleanup could not be confirmed"
            );
        }
    });
    true
}

/// Watch the local SSH child. When the SSH connection drops or the remote
/// worker exits, clean the remote Run root even if the host never calls
/// explicit shutdown. The weak reference avoids keeping the connection alive
/// and the task stops as soon as the connection is dropped.
fn spawn_disconnect_monitor(
    process: &Arc<AsyncMutex<Option<ManagedProcess>>>,
    cleanup: Arc<ConnectionCleanup>,
) -> tokio::task::JoinHandle<()> {
    let process = Arc::downgrade(process);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let Some(process) = process.upgrade() else {
                return;
            };
            let exited = {
                let guard = process.lock().await;
                guard.as_ref().map(ManagedProcess::has_exited).transpose()
            };
            match exited {
                Ok(Some(true)) | Ok(None) => {
                    if cleanup.pending()
                        && let Err(failure) = cleanup.terminate(true).await
                    {
                        tracing::warn!(
                            target: "saya::browser",
                            event = "browser.vm.cleanup_unknown",
                            code = %failure.code,
                            "Browser VM disconnected and cleanup could not be confirmed"
                        );
                    }
                    return;
                }
                Ok(Some(false)) => {}
                Err(_) => return,
            }
        }
    })
}

struct VmWorkerConnection {
    client: AsyncMutex<Option<crate::mcp::McpClient>>,
    process: Arc<AsyncMutex<Option<ManagedProcess>>>,
    protocol: String,
    generation: AtomicU64,
    pid: u32,
    stderr_tail: Arc<Mutex<VecDeque<u8>>>,
    cleanup: Arc<ConnectionCleanup>,
    monitor: AsyncMutex<Option<tokio::task::JoinHandle<()>>>,
    /// Per-worker broker. Dropping it signals shutdown even when an outer
    /// caller never reaches `WorkerConnection::shutdown`.
    network: Option<Arc<BrowserNetwork>>,
    /// Host Seatbelt environment that carries the exact broker port rule.
    host: Option<Arc<VmHostEnvironment>>,
}

impl Drop for VmWorkerConnection {
    fn drop(&mut self) {
        if self.cleanup.pending() {
            schedule_drop_cleanup(&self.cleanup);
        }
    }
}

#[async_trait]
impl WorkerConnection for VmWorkerConnection {
    fn protocol_version(&self) -> Option<String> {
        Some(self.protocol.clone())
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    fn process_id(&self) -> Option<u32> {
        Some(self.pid)
    }

    fn stderr_tail(&self) -> Vec<u8> {
        self.stderr_tail
            .lock()
            .map(|tail| tail.iter().copied().collect())
            .unwrap_or_default()
    }

    async fn process_info(
        &self,
        cancel: &dyn Cancellation,
    ) -> Result<worker::WorkerProcessInfo, BrowserFailure> {
        let client = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(BrowserFailure::new("cancelled", "Browser process information query cancelled")),
            client = self.client.lock() => client,
        };
        let client = client
            .as_ref()
            .ok_or_else(|| BrowserFailure::new("worker_closed", "Browser VM worker is closed"))?;
        let value = client
            .browser_process_info(cancel)
            .await
            .map_err(worker::process_info_failure)?;
        worker::WorkerProcessInfo::decode(value)
    }

    async fn list_tools(&self, cancel: &dyn Cancellation) -> Result<McpCatalog, BrowserFailure> {
        let client = self.client.lock().await;
        let client = client
            .as_ref()
            .ok_or_else(|| BrowserFailure::new("worker_closed", "Browser VM worker is closed"))?;
        let catalog = client.list_tools(cancel).await.map_err(map_mcp_failure)?;
        self.generation.store(catalog.generation, Ordering::Release);
        Ok(catalog)
    }

    async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        generation: u64,
        cancel: &dyn Cancellation,
    ) -> Result<McpToolResult, BrowserFailure> {
        let client = self.client.lock().await;
        let client = client
            .as_ref()
            .ok_or_else(|| BrowserFailure::new("worker_closed", "Browser VM worker is closed"))?;
        client
            .call_tool(name, arguments, generation, cancel)
            .await
            .map_err(map_mcp_failure)
    }

    async fn put_upload(
        &self,
        filename: &str,
        bytes: &[u8],
        cancel: &dyn Cancellation,
    ) -> Result<String, BrowserFailure> {
        self.cleanup
            .control
            .put_upload(filename, bytes, cancel)
            .await
    }

    async fn remove_upload(
        &self,
        path: &str,
        cancel: &dyn Cancellation,
    ) -> Result<(), BrowserFailure> {
        self.cleanup.control.remove_upload(path, cancel).await
    }

    async fn read_download(
        &self,
        path: &str,
        max_bytes: usize,
        cancel: &dyn Cancellation,
    ) -> Result<Vec<u8>, BrowserFailure> {
        self.cleanup
            .control
            .read_download(path, max_bytes, cancel)
            .await
    }

    async fn list_downloads(
        &self,
        cancel: &dyn Cancellation,
    ) -> Result<BTreeSet<String>, BrowserFailure> {
        self.cleanup.control.list_downloads(cancel).await
    }

    async fn shutdown(&self) -> Result<(), BrowserFailure> {
        let mut failure: Option<BrowserFailure> = None;
        if let Some(client) = self.client.lock().await.take()
            && client.shutdown().await.is_err()
        {
            failure = Some(remote_cleanup_failure(
                "Browser VM worker MCP shutdown could not be confirmed",
            ));
        }
        if let Some(mut process) = self.process.lock().await.take()
            && process.terminate().await.is_err()
        {
            failure = Some(remote_cleanup_failure(
                "Browser VM host SSH termination could not be confirmed",
            ));
        }
        if let Err(error) = self.cleanup.terminate(true).await {
            failure = Some(error);
        }
        // The SSH client is gone, so the reverse forward no longer exists.
        // Stop the matching broker listener and wait for its connection tasks.
        if let Some(network) = &self.network
            && let Err(error) = network.shutdown().await
        {
            failure.get_or_insert(BrowserFailure::new(error.0.code, error.0.message));
        }
        // The SSH process has been terminated. Wait for the Host channel's
        // process bookkeeping before the environment tree is dropped.
        if let Some(host) = &self.host {
            let _ = host.environment.wait_for_process_cleanup().await;
        }
        if self.cleanup.done.load(Ordering::Acquire)
            && let Some(monitor) = self.monitor.lock().await.take()
        {
            monitor.abort();
        }
        match failure {
            Some(failure) => Err(failure),
            None => Ok(()),
        }
    }
}

fn map_mcp_failure(failure: BoundaryError) -> BrowserFailure {
    match failure.0.code.as_str() {
        "outcome_unknown" | "mcp_timeout" | "mcp_schema" => BrowserFailure::uncertain(
            "Browser VM worker call completion could not be confirmed. The call was not replayed",
        ),
        "cancelled" => BrowserFailure::new(
            "cancelled",
            "Browser VM worker call cancelled before dispatch",
        ),
        "mcp_stale_catalog" => BrowserFailure::new(
            "stale_catalog",
            "Browser VM worker catalogue changed; begin a new session",
        ),
        "mcp_unknown_tool" => {
            BrowserFailure::new("worker_catalog", "Browser VM worker tool is unavailable")
        }
        "invalid_args" => BrowserFailure::new(
            "invalid_worker_args",
            "Browser VM worker rejected the host request before dispatch",
        ),
        "mcp_limit" => BrowserFailure::new(
            "worker_limit",
            "Browser VM worker request exceeded a host limit",
        ),
        "needs_input" => BrowserFailure::new(
            "worker_input",
            "Browser VM worker requested additional input, which this provider does not supply",
        ),
        _ => BrowserFailure::new(
            "worker_transport",
            "Browser VM worker call failed before a terminal result",
        ),
    }
}

/// Fixed worker arguments for the VM backend. Chromium's sandbox is always
/// enabled; no flag here disables it.
#[allow(clippy::too_many_arguments)]
fn worker_launch_arguments(
    run_root: &str,
    proxy_socket: &str,
    proxy_port: u16,
    pid_file: &str,
    node: &str,
    cli_js: &str,
    config_path: &str,
    chromium: &str,
) -> Vec<String> {
    vec![
        run_root.to_owned(),
        proxy_socket.to_owned(),
        proxy_port.to_string(),
        pid_file.to_owned(),
        node.to_owned(),
        cli_js.to_owned(),
        "--config".into(),
        config_path.to_owned(),
        "--headless".into(),
        "--isolated".into(),
        "--sandbox".into(),
        "--block-service-workers".into(),
        "--no-webmcp".into(),
        "--ignore-roots".into(),
        "--image-responses".into(),
        "omit".into(),
        "--output-dir".into(),
        format!("{run_root}/output"),
        "--executable-path".into(),
        chromium.to_owned(),
    ]
}

/// Host-side factory for the per-worker browser broker. It owns the single
/// bounded observation log and the single Run-scoped budget exposed by the
/// provider. Every worker lifecycle and broker restart reuses the same
/// cumulative bytes, connection attempts and fixed deadline.
pub struct BrowserNetworkFactory {
    settings: BrowserNetworkSettings,
    log: Arc<BrowserNetworkLog>,
    budget: Arc<RunNetworkBudget>,
}

impl BrowserNetworkFactory {
    pub fn new(settings: BrowserNetworkSettings, run_id: &str) -> Result<Self, BoundaryError> {
        settings.validate()?;
        if !settings.enabled {
            return Err(invalid_config("Browser network is disabled"));
        }
        let budget = RunNetworkBudget::new(&settings.limits)
            .map_err(|_| invalid_config("Browser network budget is invalid"))?;
        Ok(Self {
            settings,
            log: Arc::new(BrowserNetworkLog::new(
                run_id,
                "vm-starting",
                MAX_NETWORK_OBSERVATIONS,
            )),
            budget,
        })
    }

    pub fn observation_log(&self) -> Arc<BrowserNetworkLog> {
        self.log.clone()
    }

    async fn start(&self, cancel: &dyn Cancellation) -> Result<BrowserNetwork, BoundaryError> {
        if cancel.is_cancelled() {
            return Err(BoundaryError(
                ErrorInfo {
                    code: "cancelled".into(),
                    message: "Browser network start cancelled".into(),
                },
                None,
                None,
            ));
        }
        // The listener is stopped by explicit shutdown or by dropping the
        // per-worker BrowserNetwork; the broker's own cancel token is not the
        // only owner of that lifetime. NeverCancel keeps the borrowed launch
        // cancellation out of a 'static task. The budget is intentionally
        // shared: restarting the worker must not reset spent capacity.
        BrowserNetwork::start_with_log_and_budget(
            &self.settings,
            self.log.clone(),
            self.budget.clone(),
            Arc::new(NeverCancel),
        )
        .await
    }
}

/// Production launcher. Creates a unique 0700 remote Run root, starts the
/// pinned worker in its own session/process group with a PID file, and maps
/// local SSH termination plus remote cleanup to a retryable unknown outcome.
pub struct VmWorkerLauncher {
    settings: BrowserVmSettings,
    runner: Arc<dyn VmCommandRunner>,
    config: ExecConfig,
    /// Per-Run broker factory. Every worker lifecycle starts a fresh listener
    /// that shares one bounded observation log with the provider.
    network: Arc<BrowserNetworkFactory>,
    /// Remote cleanup handles that could not be confirmed yet. They remain
    /// registered even when no connection was established, so a later launch
    /// or shutdown can retry instead of dropping the Run root silently. The
    /// entry is registered before the first await of a launch (and before a
    /// cleanup await), so dropping the launch future cannot lose the only
    /// handle to the remote directory.
    pending_cleanup: Mutex<VecDeque<Arc<PendingCleanup>>>,
}

struct PendingCleanup {
    control: Arc<dyn VmRemoteControl>,
    /// Observational only: whether a worker may have been started for this Run
    /// root. It selects the cleanup path but never blocks a safe cleanup.
    expect_worker: AtomicBool,
}

impl VmWorkerLauncher {
    pub fn new(
        settings: BrowserVmSettings,
        runner: Arc<dyn VmCommandRunner>,
        config: ExecConfig,
        network: Arc<BrowserNetworkFactory>,
    ) -> Self {
        Self {
            settings,
            runner,
            config,
            network,
            pending_cleanup: Mutex::new(VecDeque::new()),
        }
    }

    /// Pending cleanup is only touched synchronously. No guard is held across
    /// an await, so an outer timeout that drops a cleanup or launch future
    /// leaves every registered handle intact.
    fn pending_entries(&self) -> std::sync::MutexGuard<'_, VecDeque<Arc<PendingCleanup>>> {
        match self.pending_cleanup.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Register a Run root before the first remote await. Registering an
    /// already-known handle updates its observation instead of duplicating it.
    fn register_pending_cleanup(&self, control: Arc<dyn VmRemoteControl>, expect_worker: bool) {
        let mut entries = self.pending_entries();
        if let Some(existing) = entries
            .iter()
            .find(|entry| Arc::ptr_eq(&entry.control, &control))
        {
            existing
                .expect_worker
                .store(expect_worker, Ordering::Release);
            return;
        }
        entries.push_back(Arc::new(PendingCleanup {
            control,
            expect_worker: AtomicBool::new(expect_worker),
        }));
    }

    fn update_pending_expect(&self, control: &Arc<dyn VmRemoteControl>, expect_worker: bool) {
        let entries = self.pending_entries();
        if let Some(existing) = entries
            .iter()
            .find(|entry| Arc::ptr_eq(&entry.control, control))
        {
            existing
                .expect_worker
                .store(expect_worker, Ordering::Release);
        }
    }

    fn remove_pending_cleanup(&self, control: &Arc<dyn VmRemoteControl>) {
        self.pending_entries()
            .retain(|entry| !Arc::ptr_eq(&entry.control, control));
    }

    fn remote_root(&self) -> Result<String, BoundaryError> {
        let root = self
            .settings
            .remote_temp_root
            .as_deref()
            .ok_or_else(|| invalid_config("Browser VM remote temp root is missing"))?;
        // Keep the per-run component short: Playwright's Unix socket paths are
        // limited to 103 bytes. The trailing 16 hex characters of the UUIDv7
        // are its random half; the leading half is timestamp-derived and would
        // collide for runs started in the same millisecond.
        let id = uuid::Uuid::now_v7().simple().to_string();
        Ok(format!(
            "{}/saya-browser-vm-{}",
            root.trim_end_matches('/'),
            &id[16..]
        ))
    }

    async fn cleanup(
        &self,
        control: &dyn VmRemoteControl,
        expect_worker: bool,
    ) -> Result<(), BrowserFailure> {
        control.terminate(expect_worker).await
    }

    /// Perform cleanup and, if it cannot be confirmed, keep the handle so a
    /// later retry sees the same Run root instead of a fresh false success.
    /// The handle is registered before the cleanup await, so dropping this
    /// future retains it too.
    async fn cleanup_or_retain(
        &self,
        control: Arc<dyn VmRemoteControl>,
        expect_worker: bool,
        failure: BrowserFailure,
    ) -> BrowserFailure {
        self.register_pending_cleanup(control.clone(), expect_worker);
        match self.cleanup(control.as_ref(), expect_worker).await {
            Ok(()) => {
                self.remove_pending_cleanup(&control);
                failure
            }
            Err(_) => remote_cleanup_failure(
                "Browser VM launch failed and Run root cleanup is unknown; the handle was retained for retry",
            ),
        }
    }
    #[allow(clippy::too_many_lines)]
    async fn launch_with_network(
        &self,
        request: WorkerLaunchRequest,
        cancel: &dyn Cancellation,
        network: Arc<BrowserNetwork>,
    ) -> Result<Arc<dyn WorkerConnection>, BrowserFailure> {
        let paths = self.settings.preflight_local().map_err(|_| {
            vm_unavailable("Browser VM local inputs are unavailable").into_browser_failure()
        })?;
        let run_root = self.remote_root().map_err(|_| {
            vm_unavailable("Browser VM remote temp root is invalid").into_browser_failure()
        })?;
        let Some(identity) = request.identity.clone() else {
            return Err(remote_failure(
                "Browser VM launch has no verified worker identity",
            ));
        };
        if self.settings.identity_sha256.as_deref() != Some(identity.manifest_sha256.as_str()) {
            return Err(identity_failure(
                "Browser VM verified identity does not match the host input",
            ));
        }
        let Some(node) = identity.node.to_str().map(str::to_owned) else {
            return Err(identity_failure(
                "Browser VM verified node path is not UTF-8",
            ));
        };
        let Some(chromium) = identity.chromium.to_str().map(str::to_owned) else {
            return Err(identity_failure(
                "Browser VM verified Chromium path is not UTF-8",
            ));
        };
        let Some(cli_js) = identity.cli_js.to_str().map(str::to_owned) else {
            return Err(identity_failure(
                "Browser VM verified worker CLI path is not UTF-8",
            ));
        };
        // The SSH process may reach only the VM SSH port and this Run's broker
        // port. The host environment is created after the broker so the
        // loopback port is exact rather than a range.
        let host = Arc::new(
            VmHostEnvironment::create(&self.settings, Some(network.endpoint_port())).map_err(
                |_| {
                    vm_unavailable("Browser VM host channel cannot be prepared")
                        .into_browser_failure()
                },
            )?,
        );
        let config_path = format!("{run_root}/config/config.json");
        let pid_file = format!("{run_root}/worker.pid");
        let proxy_socket = format!("{run_root}/proxy.sock");

        let control: Arc<dyn VmRemoteControl> = Arc::new(RunnerRemoteControl {
            settings: self.settings.clone(),
            runner: self.runner.clone(),
            run_root: run_root.clone(),
            pid_file: pid_file.clone(),
            cli_js: cli_js.clone(),
        });
        // The Run root exists from this point on. Register the cleanup handle
        // before the first prepare/launch await so an outer cancellation or a
        // dropped launch future cannot lose the remote directory.
        self.register_pending_cleanup(control.clone(), false);

        let vm_proxy_url = format!("http://127.0.0.1:{}", super::network::VM_PROXY_PORT);
        let config =
            worker_config_json(&self.settings, &run_root, &vm_proxy_url).map_err(|_| {
                vm_unavailable("Browser VM worker config is invalid").into_browser_failure()
            })?;
        let prepare_args = [run_root.clone()];
        let prepared = vm_command_result(
            self.runner
                .run(
                    VmCommandSpec {
                        script: PREPARE_RUN_SCRIPT,
                        args: &prepare_args,
                        stdin: Some(&config),
                        timeout: Duration::from_millis(self.settings.connect_timeout_ms),
                        stderr_limit: 16 * 1024,
                        stdout_limit: 512 * 1024,
                    },
                    cancel,
                )
                .await,
            "Browser VM Run root preparation",
        );
        match prepared {
            Ok(output)
                if output.succeeded()
                    && String::from_utf8_lossy(&output.stdout).trim() == run_root => {}
            Ok(_) => {
                let failure = remote_failure("Browser VM Run root could not be prepared");
                return Err(self
                    .cleanup_or_retain(control.clone(), false, failure)
                    .await);
            }
            Err(failure) if failure.code == "cancelled" => return Err(failure),
            Err(failure) => {
                return Err(self
                    .cleanup_or_retain(control.clone(), false, failure)
                    .await);
            }
        }
        if cancel.is_cancelled() {
            let failure = BrowserFailure::new(
                "cancelled",
                "Browser VM launch cancelled before worker start",
            );
            return Err(self
                .cleanup_or_retain(control.clone(), false, failure)
                .await);
        }

        let launch_args = worker_launch_arguments(
            &run_root,
            &proxy_socket,
            super::network::VM_PROXY_PORT,
            &pid_file,
            &node,
            &cli_js,
            &config_path,
            &chromium,
        );
        // The host channel's generated SSH config is written by the runner for
        // short commands; the long-lived launch needs its own copy.
        let ssh_config = match strict_ssh_config(&self.settings, &paths) {
            Ok(config) => config,
            Err(_) => {
                let failure =
                    vm_unavailable("Browser VM SSH config is invalid").into_browser_failure();
                return Err(self
                    .cleanup_or_retain(control.clone(), false, failure)
                    .await);
            }
        };
        let ssh_config_path = host.directories.config.join("ssh_config");
        if write_private(&ssh_config_path, ssh_config.as_bytes()).is_err() {
            let failure = remote_failure("Browser VM SSH config could not be written");
            return Err(self
                .cleanup_or_retain(control.clone(), false, failure)
                .await);
        }
        let argv = match ssh_argv(
            &self.settings,
            &paths,
            &ssh_config_path,
            LAUNCH_SCRIPT,
            &launch_args,
            Some(&ReverseForward {
                remote_socket: proxy_socket.clone(),
                local_port: network.endpoint_port(),
            }),
        ) {
            Ok(argv) => argv,
            Err(_) => {
                let failure =
                    vm_unavailable("Browser VM SSH launch input is invalid").into_browser_failure();
                return Err(self
                    .cleanup_or_retain(control.clone(), false, failure)
                    .await);
            }
        };

        let mut env = BTreeMap::new();
        env.insert(OsString::from("PATH"), OsString::from("/usr/bin:/bin"));
        env.insert(OsString::from("LANG"), OsString::from("C"));
        env.insert(OsString::from("NO_COLOR"), OsString::from("1"));
        env.insert(
            OsString::from("HOME"),
            host.directories.work.clone().into_os_string(),
        );
        // From here on a remote worker may have been started. Record that
        // observation before spawning the SSH process so a dropped launch
        // future retries with the scanning cleanup path.
        self.update_pending_expect(&control, true);
        let spawned = match spawn_remote_worker(
            &argv,
            &host.directories.work,
            env,
            host.environment.clone(),
            &self.config,
            request.timeout,
            request.stderr_limit,
        ) {
            Ok(spawned) => spawned,
            Err(_) => {
                let failure = remote_failure("Browser VM SSH worker could not start");
                return Err(self
                    .cleanup_or_retain(control.clone(), false, failure)
                    .await);
            }
        };

        // Transfer stdin/stdout to the MCP client before issuing an independent
        // PID query. The protocol channel owns the pipes for the entire check.
        let VmSpawned {
            process,
            stdin,
            stdout,
            stderr_tail,
            stderr_task,
        } = spawned;
        let client = match crate::mcp::McpClient::connect_stdio(
            "saya-browser-vm-worker",
            stdout,
            stdin,
            request.mcp_limits,
            cancel,
        )
        .await
        {
            Ok(client) => client,
            Err(_) => {
                let mut process = process;
                let _ = process.terminate().await;
                // The abort path above may race the reader; wait for the
                // bounded drain before inspecting the tail so a `network=`
                // sentinel is not lost.
                let stderr = drain_stderr_bounded(&stderr_tail, stderr_task).await;
                let failure = classify_launch_failure(cancel, &stderr);
                return Err(self.cleanup_or_retain(control.clone(), true, failure).await);
            }
        };
        let protocol = client.protocol_version().unwrap_or_default();

        // Read the remote PID from the worker's own session/group. A missing or
        // mismatched PID is never accepted as a successful launch.
        let pid_args = [pid_file.clone(), node, cli_js.clone()];
        let pid_output = vm_command_result(
            self.runner
                .run(
                    VmCommandSpec {
                        script: PID_SCRIPT,
                        args: &pid_args,
                        stdin: None,
                        timeout: Duration::from_millis(self.settings.connect_timeout_ms),
                        stderr_limit: 16 * 1024,
                        stdout_limit: 512 * 1024,
                    },
                    cancel,
                )
                .await,
            "Browser VM worker PID query",
        );
        let remote_pid = match pid_output {
            Ok(ref output) if output.succeeded() => parse_pid(&output.stdout),
            _ => None,
        };
        let Some(remote_pid) = remote_pid else {
            drop(client);
            let mut process = process;
            let _ = process.terminate().await;
            let failure = remote_failure("Browser VM worker PID could not be confirmed");
            return Err(self.cleanup_or_retain(control.clone(), true, failure).await);
        };
        network.set_worker_pid(remote_pid);
        // The connection takes over the remote Run root, the broker and the
        // host channel environment. Removing the pending entry and
        // constructing the connection happen without an intervening await, so
        // a dropped future cannot leave both an orphaned root and no owner.
        self.remove_pending_cleanup(&control);
        let cleanup = Arc::new(ConnectionCleanup::new(control));
        let process = Arc::new(AsyncMutex::new(Some(process)));
        let monitor = spawn_disconnect_monitor(&process, cleanup.clone());
        Ok(Arc::new(VmWorkerConnection {
            client: AsyncMutex::new(Some(client)),
            process,
            protocol,
            generation: AtomicU64::new(0),
            pid: remote_pid,
            stderr_tail,
            cleanup,
            monitor: AsyncMutex::new(Some(monitor)),
            network: Some(network),
            host: Some(host),
        }))
    }
}

#[async_trait]
impl WorkerLauncher for VmWorkerLauncher {
    async fn launch(
        &self,
        request: WorkerLaunchRequest,
        cancel: &dyn Cancellation,
    ) -> Result<Arc<dyn WorkerConnection>, BrowserFailure> {
        if cancel.is_cancelled() {
            return Err(BrowserFailure::new(
                "cancelled",
                "Browser VM launch cancelled before dispatch",
            ));
        }
        if request.execution.is_none() {
            return Err(
                vm_unavailable("Browser VM requires a prepared host channel")
                    .into_browser_failure(),
            );
        }
        // Start the Run's broker before the VM SSH channel exists so the
        // reverse forward can target its loopback port. Any failure after this
        // point shuts the listener down; there is no default-VM-egress path.
        let network = match self.network.start(cancel).await {
            Ok(network) => Arc::new(network),
            Err(error) => {
                return Err(BrowserFailure::new(error.0.code, error.0.message));
            }
        };
        match self
            .launch_with_network(request, cancel, network.clone())
            .await
        {
            Ok(connection) => Ok(connection),
            Err(failure) => {
                network.shutdown_best_effort().await;
                Err(failure)
            }
        }
    }

    async fn retry_pending_cleanup(&self) -> Result<(), BrowserFailure> {
        // Peek one entry at a time and remove only that entry after a
        // confirmed cleanup. An outer timeout, cancellation or dropped future
        // leaves the current entry and every later one registered.
        loop {
            let item = {
                let entries = self.pending_entries();
                entries.front().cloned()
            };
            let Some(item) = item else {
                return Ok(());
            };
            let expect_worker = item.expect_worker.load(Ordering::Acquire);
            match self.cleanup(item.control.as_ref(), expect_worker).await {
                Ok(()) => {
                    let mut entries = self.pending_entries();
                    if entries
                        .front()
                        .is_some_and(|front| Arc::ptr_eq(front, &item))
                    {
                        entries.pop_front();
                    } else {
                        entries.retain(|entry| !Arc::ptr_eq(entry, &item));
                    }
                }
                Err(failure) => return Err(failure),
            }
        }
    }
}

fn parse_pid(output: &[u8]) -> Option<u32> {
    let text = std::str::from_utf8(output).ok()?;
    let mut pid = None;
    let mut cmdline = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("pid=") {
            pid = value.parse::<u32>().ok();
        } else if let Some(value) = line.strip_prefix("cmdline=") {
            cmdline = Some(value);
        }
    }
    let pid = pid?;
    if pid <= 1 {
        return None;
    }
    let cmdline = cmdline?;
    if cmdline.contains("--no-sandbox") || cmdline.contains("--disable-setuid-sandbox") {
        return None;
    }
    Some(pid)
}

// ---------------------------------------------------------------------------
// Environment factory and sandbox evidence.
// ---------------------------------------------------------------------------

/// Factory over the host SSH sandbox. Every lifecycle gets a fresh run-private
/// host tree and a fresh Seatbelt environment.
pub struct VmBrowserEnvironmentFactory {
    settings: BrowserVmSettings,
}

impl VmBrowserEnvironmentFactory {
    pub fn new(settings: BrowserVmSettings) -> Result<Self, BoundaryError> {
        settings.preflight_local()?;
        // Preflight once so an unsupported host fails before any worker starts.
        let environment = VmHostEnvironment::create(&settings, None)?;
        drop(environment);
        Ok(Self { settings })
    }
}

impl BrowserEnvironmentFactory for VmBrowserEnvironmentFactory {
    fn prepare(&self) -> Result<Arc<dyn BrowserWorkspaceLease>, BoundaryError> {
        let host = VmHostEnvironment::create(&self.settings, None)?;
        Ok(Arc::new(VmBrowserWorkspaceLease { host }))
    }
}

struct VmBrowserWorkspaceLease {
    host: VmHostEnvironment,
}

#[async_trait]
impl BrowserWorkspaceLease for VmBrowserWorkspaceLease {
    fn directories(&self) -> &BrowserSandboxDirectories {
        &self.host.directories
    }

    fn execution(&self) -> Option<PreparedEnvironment> {
        Some(self.host.environment.clone())
    }

    async fn release(&self) -> Result<(), BoundaryError> {
        self.host.environment.wait_for_process_cleanup().await?;
        self.host.environment.cleanup().map(|_| ())?;
        match fs::remove_dir_all(&self.host.base) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(BoundaryError(
                ErrorInfo {
                    code: "worker_cleanup".into(),
                    message: "Browser VM host channel cleanup could not be completed".into(),
                },
                None,
                None,
            )),
        }
    }
}

/// Verifies Chromium's sandbox from inside the VM. The worker PID is the
/// remote process-group leader; host `/proc` is never consulted.
pub struct VmChromiumSandboxInspector {
    settings: BrowserVmSettings,
    runner: Arc<dyn VmCommandRunner>,
}

impl VmChromiumSandboxInspector {
    pub fn new(settings: BrowserVmSettings, runner: Arc<dyn VmCommandRunner>) -> Self {
        Self { settings, runner }
    }
}

#[async_trait]
impl ChromiumSandboxInspector for VmChromiumSandboxInspector {
    async fn inspect(
        &self,
        worker_pid: u32,
        stderr_tail: &[u8],
        cancel: &dyn Cancellation,
    ) -> Result<ChromiumSandboxEvidence, BrowserFailure> {
        let args = [worker_pid.to_string()];
        let output = vm_command_result(
            self.runner
                .run(
                    VmCommandSpec {
                        script: SANDBOX_SCRIPT,
                        args: &args,
                        stdin: None,
                        timeout: Duration::from_millis(
                            self.settings.cleanup_timeout_ms.max(15_000),
                        ),
                        stderr_limit: 16 * 1024,
                        stdout_limit: 512 * 1024,
                    },
                    cancel,
                )
                .await,
            "Browser VM sandbox inspection",
        )?;
        if !output.succeeded() {
            return Err(BrowserFailure::new(
                "browser_sandbox_unavailable",
                "Browser VM sandbox inspection did not complete",
            ));
        }
        let text = std::str::from_utf8(&output.stdout).unwrap_or_default();
        let mut renderer = None;
        let mut cmdline = None;
        let mut no_new_privs = false;
        let mut seccomp = 0u32;
        for line in text.lines() {
            if let Some(value) = line.strip_prefix("renderer=") {
                renderer = value.parse::<u32>().ok();
            } else if let Some(value) = line.strip_prefix("cmdline=") {
                cmdline = Some(value.to_owned());
            } else if let Some(value) = line.strip_prefix("nonewprivs=") {
                no_new_privs = value.trim() == "1";
            } else if let Some(value) = line.strip_prefix("seccomp=") {
                seccomp = value.trim().parse::<u32>().unwrap_or(0);
            }
        }
        let Some(renderer) = renderer else {
            return Err(BrowserFailure::new(
                "browser_sandbox_unavailable",
                "Browser VM produced no Chromium renderer evidence",
            ));
        };
        let Some(cmdline) = cmdline else {
            return Err(BrowserFailure::new(
                "browser_sandbox_unavailable",
                "Browser VM renderer command line is unavailable",
            ));
        };
        let sandbox_arguments_valid =
            !cmdline.contains("--no-sandbox") && !cmdline.contains("--disable-setuid-sandbox");
        if !sandbox_arguments_valid || !no_new_privs || seccomp != 2 {
            return Err(BrowserFailure::new(
                "browser_sandbox_unavailable",
                "Chromium sandbox evidence inside the VM is insufficient",
            ));
        }
        Ok(ChromiumSandboxEvidence {
            renderer_pid: renderer,
            no_new_privs,
            seccomp_mode: seccomp,
            sandbox_arguments_valid,
            stderr_initializes_sandbox: stderr_tail
                .windows(b"InitializeSandbox".len())
                .any(|window| window == b"InitializeSandbox"),
        })
    }
}

// ---------------------------------------------------------------------------
// Assembly.
// ---------------------------------------------------------------------------

/// Assemble the production VM provider. The caller has already selected the VM
/// backend explicitly; there is no fallback to the native or Host path.
pub fn assemble_vm_browser_provider(
    settings: BrowserVmSettings,
    network_settings: BrowserNetworkSettings,
    run_id: &str,
    exec: ExecConfig,
) -> Result<super::worker::PlaywrightMcpProvider, BoundaryError> {
    settings.preflight_local()?;
    let network = Arc::new(BrowserNetworkFactory::new(network_settings, run_id)?);
    let observation_log = network.observation_log();
    let runner: Arc<dyn VmCommandRunner> =
        Arc::new(SshVmCommandRunner::new(settings.clone())?.with_exec_config(exec.clone()));
    let verifier = Arc::new(VmWorkerIdentityVerifier::new(
        settings.clone(),
        runner.clone(),
    ));
    let planner = Arc::new(VmWorkerLaunchPlanner::new(settings.clone()));
    let launcher: Arc<dyn WorkerLauncher> = Arc::new(VmWorkerLauncher::new(
        settings.clone(),
        runner.clone(),
        exec,
        network,
    ));
    let factory = Arc::new(VmBrowserEnvironmentFactory::new(settings.clone())?);
    let inspector = Arc::new(VmChromiumSandboxInspector::new(settings, runner));
    Ok(super::worker::PlaywrightMcpProvider::with_worker_backend(
        "vm-colima",
        verifier,
        planner,
        launcher,
        factory,
        inspector,
    )?
    .with_network_log(observation_log))
}

impl BoundaryError {
    fn into_browser_failure(self) -> BrowserFailure {
        BrowserFailure::new(self.0.code, self.0.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::BackendKind;
    fn test_network_factory() -> Arc<BrowserNetworkFactory> {
        Arc::new(
            BrowserNetworkFactory::new(
                BrowserNetworkSettings {
                    enabled: true,
                    allowed_origins: vec!["https://example.com".into()],
                    ..Default::default()
                },
                "test-run",
            )
            .unwrap(),
        )
    }

    struct Cancelled;
    impl Cancellation for Cancelled {
        fn is_cancelled(&self) -> bool {
            true
        }
        fn cancelled(
            &self,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
            Box::pin(std::future::ready(()))
        }
    }

    use serde_json::json;
    #[cfg(target_os = "macos")]
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn upload_script_preserves_original_name_and_cleans_partial_staging() {
        use std::{
            io::Write as _,
            process::{Command, Stdio},
        };

        let root = tempfile::tempdir().unwrap();
        let root_text = root.path().to_str().unwrap().to_owned();
        let run = |name: &str, input: &[u8]| {
            let mut child = Command::new("sh")
                .arg("-c")
                .arg(UPLOAD_SCRIPT)
                .arg("sh")
                .arg(&root_text)
                .arg(name)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(input).unwrap();
            child.wait_with_output().unwrap()
        };

        let output = run("invoice 2026.txt", b"payload");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            root.path()
                .join("uploads/invoice 2026.txt")
                .to_str()
                .unwrap()
        );
        assert_eq!(
            std::fs::read(root.path().join("uploads/invoice 2026.txt")).unwrap(),
            b"payload"
        );

        // A `..` inside a basename is not traversal and must be preserved.
        let output = run("archive..tar", b"dots");
        assert!(output.status.success());
        assert_eq!(
            std::fs::read(root.path().join("uploads/archive..tar")).unwrap(),
            b"dots"
        );

        // A refused move must remove the temporary file, not leave a partial.
        std::fs::create_dir(root.path().join("uploads/blocked.txt")).unwrap();
        let output = run("blocked.txt", b"payload");
        assert!(!output.status.success());
        let partial: Vec<String> = std::fs::read_dir(root.path().join("uploads"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".upload."))
            .collect();
        assert!(partial.is_empty(), "partial staging remained: {partial:?}");

        for name in ["../escape.txt", "dir/name.txt", ".hidden", "-rf", ".."] {
            assert!(!run(name, b"x").status.success(), "{name} was accepted");
        }
    }

    use crate::network::BrokerLimits;

    fn digest(bytes: &[u8]) -> String {
        sha256_hex(bytes)
    }

    fn package(name: &str, version: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({"name": name, "version": version})).unwrap()
    }

    #[test]
    fn pinned_headless_shell_path_accepts_the_actual_linux_arm_layout() {
        assert!(headless_shell_path(
            "/opt/saya-browser/chromium/chrome-headless-shell-linux-arm64/chrome-headless-shell"
                .replace("chromium/", "chromium_headless_shell-1254/")
                .as_str()
        ));
        assert!(headless_shell_path(
            "/opt/saya/browsers/chromium_headless_shell-1254/chrome-headless-shell-linux64/chrome-headless-shell"
        ));
        assert!(!headless_shell_path(
            "/opt/saya-browser/chromium_headless_shell-1254/chrome-headless-shell-linux-arm64/chrome"
        ));
        assert!(!headless_shell_path(
            "/opt/saya-browser/chromium_headless_shell-9999/chrome-headless-shell-linux-arm64/chrome-headless-shell"
        ));
    }

    struct IdentityFixture {
        settings: BrowserVmSettings,
        probe: Vec<u8>,
        manifest: Vec<u8>,
        mcp_package: Vec<u8>,
        playwright_package: Vec<u8>,
        playwright_core_package: Vec<u8>,
        browsers: Vec<u8>,
        installation_probe: Vec<u8>,
        node_version: Vec<u8>,
    }

    impl IdentityFixture {
        fn new() -> Self {
            let manifest_path = "/opt/saya/identity.json";
            let node_path = "/opt/saya/node/bin/node";
            let worker_root = "/opt/saya/worker";
            let cli_path = format!("{worker_root}/{}", worker::CLI_JS_PATH);
            let mcp_path = format!("{worker_root}/{}", worker::MCP_PACKAGE_PATH);
            let playwright_path = format!("{worker_root}/{}", worker::PLAYWRIGHT_PACKAGE_PATH);
            let core_path = format!("{worker_root}/{}", worker::PLAYWRIGHT_CORE_PACKAGE_PATH);
            let browsers_path = format!("{worker_root}/{}", worker::BROWSERS_JSON_PATH);
            let chromium_path = "/opt/saya/browsers/chromium_headless_shell-1254/chrome-headless-shell-linux64/chrome-headless-shell";

            let node = b"pinned node".to_vec();
            let cli = b"// pinned worker\n".to_vec();
            let mcp_package = package("@playwright/mcp", PLAYWRIGHT_MCP_VERSION);
            let playwright_package = package("playwright", PLAYWRIGHT_VERSION);
            let playwright_core_package = package("playwright-core", PLAYWRIGHT_VERSION);
            let browsers = serde_json::to_vec(&json!({
                "browsers": [{
                    "name": "chromium",
                    "revision": CHROMIUM_REVISION.to_string(),
                    "browserVersion": CHROMIUM_VERSION,
                }]
            }))
            .unwrap();
            let chromium = b"pinned chromium".to_vec();
            let installation_trees = json!({
                "node": {"sha256": digest(b"node installation"), "entries": 2, "bytes": node.len()},
                "worker": {"sha256": digest(b"worker installation"), "entries": 12, "bytes": cli.len() + mcp_package.len() + playwright_package.len() + playwright_core_package.len() + browsers.len()},
                "chromium": {"sha256": digest(b"engine installation"), "entries": 2, "bytes": chromium.len()},
            });
            let manifest_bytes = serde_json::to_vec_pretty(&json!({
                "schema_version": 2,
                "installation_trees": installation_trees,
                "node": {"sha256": digest(&node), "size": node.len(), "version": "v22.23.0"},
                "worker_cli_js": {"sha256": digest(&cli), "size": cli.len()},
                "mcp_package": {"sha256": digest(&mcp_package), "size": mcp_package.len(), "version": PLAYWRIGHT_MCP_VERSION},
                "playwright_package": {"sha256": digest(&playwright_package), "size": playwright_package.len(), "version": PLAYWRIGHT_VERSION},
                "playwright_core_package": {"sha256": digest(&playwright_core_package), "size": playwright_core_package.len(), "version": PLAYWRIGHT_VERSION},
                "playwright_core_browsers_json": {"sha256": digest(&browsers), "size": browsers.len()},
                "chromium": {"sha256": digest(&chromium), "size": chromium.len(), "version": CHROMIUM_VERSION, "revision": CHROMIUM_REVISION},
            }))
            .unwrap();
            let manifest_digest = digest(&manifest_bytes);

            let entries = [
                (
                    manifest_path,
                    manifest_bytes.len(),
                    0o600u32,
                    digest(&manifest_bytes),
                ),
                (node_path, node.len(), 0o755, digest(&node)),
                (cli_path.as_str(), cli.len(), 0o644, digest(&cli)),
                (
                    mcp_path.as_str(),
                    mcp_package.len(),
                    0o644,
                    digest(&mcp_package),
                ),
                (
                    playwright_path.as_str(),
                    playwright_package.len(),
                    0o644,
                    digest(&playwright_package),
                ),
                (
                    core_path.as_str(),
                    playwright_core_package.len(),
                    0o644,
                    digest(&playwright_core_package),
                ),
                (
                    browsers_path.as_str(),
                    browsers.len(),
                    0o644,
                    digest(&browsers),
                ),
                (chromium_path, chromium.len(), 0o755, digest(&chromium)),
            ];
            let mut probe = Vec::new();
            for (_, size, mode, hash) in &entries {
                probe.extend_from_slice(
                    format!("{size}|{mode:o}|regular file\n{hash}  file\n").as_bytes(),
                );
            }
            let settings = BrowserVmSettings {
                enabled: true,
                colima: Some(PathBuf::from("/usr/bin/ssh")),
                ssh: Some(PathBuf::from("/usr/bin/ssh")),
                profile: Some("saya-browser".into()),
                host: Some("127.0.0.1".into()),
                user: Some("saya".into()),
                port: Some(2222),
                identity_file: Some(PathBuf::from("/tmp/saya-test-key")),
                known_hosts: Some(PathBuf::from("/tmp/saya-test-known-hosts")),
                node: Some(node_path.into()),
                worker_root: Some(worker_root.into()),
                chromium: Some(chromium_path.into()),
                identity_manifest: Some(manifest_path.into()),
                identity_sha256: Some(manifest_digest),
                remote_temp_root: Some("/tmp/saya-runs".into()),
                ..Default::default()
            };
            Self {
                settings,
                probe,
                manifest: manifest_bytes,
                mcp_package,
                playwright_package,
                playwright_core_package,
                browsers,
                installation_probe: serde_json::to_vec(&installation_trees).unwrap(),
                node_version: b"v22.23.0\n".to_vec(),
            }
        }

        fn outputs(&self) -> VecDeque<VmCommandOutput> {
            [
                self.probe.clone(),
                self.manifest.clone(),
                self.mcp_package.clone(),
                self.playwright_package.clone(),
                self.playwright_core_package.clone(),
                self.browsers.clone(),
                self.installation_probe.clone(),
                self.node_version.clone(),
            ]
            .into_iter()
            .map(|stdout| VmCommandOutput {
                status: Some(0),
                stdout,
                stderr: Vec::new(),
                timed_out: false,
                cancelled: false,
            })
            .collect()
        }
    }

    struct QueueRunner {
        outputs: Mutex<VecDeque<VmCommandOutput>>,
        scripts: Mutex<Vec<String>>,
    }

    impl QueueRunner {
        fn new(outputs: VecDeque<VmCommandOutput>) -> Arc<Self> {
            Arc::new(Self {
                outputs: Mutex::new(outputs),
                scripts: Mutex::new(Vec::new()),
            })
        }

        fn calls(&self) -> usize {
            self.scripts.lock().unwrap().len()
        }

        fn scripts(&self) -> Vec<String> {
            self.scripts.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl VmCommandRunner for QueueRunner {
        async fn run(
            &self,
            spec: VmCommandSpec<'_>,
            _cancel: &dyn Cancellation,
        ) -> Result<VmCommandOutput, BrowserFailure> {
            self.scripts.lock().unwrap().push(spec.script.to_owned());
            self.outputs
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| BrowserFailure::new("test_runner", "no queued output"))
        }
    }

    fn success(stdout: impl Into<Vec<u8>>) -> VmCommandOutput {
        VmCommandOutput {
            status: Some(0),
            stdout: stdout.into(),
            stderr: Vec::new(),
            timed_out: false,
            cancelled: false,
        }
    }

    #[test]
    fn vm_settings_default_disabled_and_reject_partial_enablement() {
        let settings = BrowserVmSettings::default();
        assert!(!settings.enabled);
        settings.validate().unwrap();
        let enabled = BrowserVmSettings {
            enabled: true,
            ..Default::default()
        };
        assert!(enabled.validate().is_err());
        let relative = BrowserVmSettings {
            ssh: Some(PathBuf::from("ssh")),
            ..Default::default()
        };
        assert!(relative.validate().is_err());
    }

    #[test]
    fn rejects_shell_injection_and_non_loopback_inputs() {
        let mut settings = IdentityFixture::new().settings;
        for bad in [
            "/opt/worker; rm -rf /",
            "/opt/$(id)",
            "/opt/`id`",
            "/opt/a\nb",
            "/opt/a b; c",
            "/opt/../root",
            "relative/path",
        ] {
            settings.node = Some(bad.into());
            assert!(settings.validate().is_err(), "{bad}");
        }
        settings.node = Some("/opt/node".into());
        settings.user = Some("root@evil".into());
        assert!(settings.validate().is_err());
        settings.user = Some("saya".into());
        settings.host = Some("example.com".into());
        assert!(settings.validate().is_err());
        settings.host = Some("127.0.0.1".into());
        settings.profile = Some("../escape".into());
        assert!(settings.validate().is_err());
        settings.profile = Some("saya-browser".into());
        settings.ssh = Some(PathBuf::from("/tmp/ssh\nProxyCommand=evil"));
        assert!(settings.validate().is_err());
        settings.ssh = Some(PathBuf::from("/usr/bin/ssh"));
        settings.validate().unwrap();
    }

    #[test]
    fn ssh_config_is_strict_and_contains_no_secret_values() {
        let settings = IdentityFixture::new().settings;
        let paths = VmLocalPaths {
            ssh: PathBuf::from("/usr/bin/ssh"),
            identity_file: PathBuf::from("/private/tmp/saya-key"),
            known_hosts: PathBuf::from("/private/tmp/saya-known-hosts"),
            colima: PathBuf::from("/usr/local/bin/colima"),
        };
        let config = strict_ssh_config(&settings, &paths).unwrap();
        for required in [
            "BatchMode yes",
            "IdentitiesOnly yes",
            "StrictHostKeyChecking yes",
            "PasswordAuthentication no",
            "KbdInteractiveAuthentication no",
            "ForwardAgent no",
            "ClearAllForwardings yes",
            "ProxyCommand none",
            "ProxyJump none",
            "ControlMaster no",
            "UpdateHostKeys no",
        ] {
            assert!(config.contains(required), "{required}");
        }
        assert!(!config.contains("PRIVATE KEY"));
        let argv = ssh_argv(
            &settings,
            &paths,
            Path::new("/private/tmp/ssh_config"),
            PROBE_SCRIPT,
            &["/opt/file".into()],
            None,
        )
        .unwrap();
        let joined = argv.join(" ");
        assert!(joined.contains("ForwardAgent=no"));
        assert!(joined.contains("StrictHostKeyChecking=yes"));
        assert!(joined.contains("IdentitiesOnly=yes"));
        assert!(!joined.contains("ForwardAgent=yes"));
        assert!(joined.contains("sh -c '"));
    }

    #[test]
    fn reverse_streamlocal_forward_replaces_clear_all_forwardings_and_fails_closed() {
        let settings = IdentityFixture::new().settings;
        let paths = VmLocalPaths {
            ssh: PathBuf::from("/usr/bin/ssh"),
            identity_file: PathBuf::from("/private/tmp/saya-key"),
            known_hosts: PathBuf::from("/private/tmp/saya-known-hosts"),
            colima: PathBuf::from("/usr/local/bin/colima"),
        };
        let base = ssh_argv(
            &settings,
            &paths,
            Path::new("/private/tmp/ssh_config"),
            PROBE_SCRIPT,
            &[],
            None,
        )
        .unwrap();
        // Plain identity commands keep every forwarding disabled.
        assert!(base.iter().any(|value| value == "ClearAllForwardings=yes"));
        assert!(!base.iter().any(|value| value == "-R"));
        assert!(!base.iter().any(|value| value == "ExitOnForwardFailure=yes"));

        let forwarded = ssh_argv(
            &settings,
            &paths,
            Path::new("/private/tmp/ssh_config"),
            PROBE_SCRIPT,
            &[],
            Some(&ReverseForward {
                remote_socket: "/tmp/run/proxy.sock".into(),
                local_port: 41_234,
            }),
        )
        .unwrap();
        // ssh uses the first obtained value, so the strict base value must be
        // replaced rather than shadowed by a later option.
        assert!(
            !forwarded
                .iter()
                .any(|value| value == "ClearAllForwardings=yes")
        );
        assert!(
            forwarded
                .iter()
                .any(|value| value == "ClearAllForwardings=no")
        );
        assert!(
            forwarded
                .iter()
                .any(|value| value == "ExitOnForwardFailure=yes")
        );
        assert!(
            forwarded
                .iter()
                .any(|value| value == "StreamLocalBindUnlink=yes")
        );
        assert_eq!(
            forwarded.iter().filter(|value| *value == "-R").count(),
            1,
            "a browser launch adds exactly one reverse forward: {forwarded:?}"
        );
        let index = forwarded
            .iter()
            .position(|value| value == "-R")
            .expect("-R");
        assert_eq!(forwarded[index + 1], "/tmp/run/proxy.sock:127.0.0.1:41234");
    }

    #[test]
    fn host_channel_has_no_workspace_mount_and_only_vm_loopback_network() {
        let settings = IdentityFixture::new().settings;
        let workspace = Path::new("/private/tmp/saya-vm-host");
        let paths = VmLocalPaths {
            ssh: PathBuf::from("/usr/bin/ssh"),
            identity_file: PathBuf::from("/private/tmp/key"),
            known_hosts: PathBuf::from("/private/tmp/known_hosts"),
            colima: PathBuf::from("/usr/local/bin/colima"),
        };
        let policy = host_policy(&settings, workspace, &paths, None).unwrap();
        assert!(policy.read_roots.is_empty());
        assert_eq!(policy.write_roots, vec![workspace.to_path_buf()]);
        assert!(policy.browser.is_none());
        assert_eq!(policy.read_files.len(), 3);
        assert!(matches!(
            policy.network,
            NetworkPolicy::LoopbackPort { port, proxy_port, .. } if port == 2222 && proxy_port.is_none()
        ));
        assert!(!matches!(policy.network, NetworkPolicy::Proxy(_)));
        // The generic default is unchanged: only this purpose gains a
        // loopback destination, and it is never the VM's default egress.
        assert!(matches!(
            SandboxPolicy::workspace(PathBuf::from("/tmp")).network,
            NetworkPolicy::Deny
        ));
    }

    #[test]
    fn host_channel_selects_the_native_backend_for_its_platform() {
        let settings = IdentityFixture::new().settings;
        let paths = VmLocalPaths {
            ssh: "/usr/bin/ssh".into(),
            identity_file: "/private/key".into(),
            known_hosts: "/private/known_hosts".into(),
            colima: "/usr/bin/colima".into(),
        };
        let policy = host_policy(&settings, Path::new("/private/vm-host"), &paths, None).unwrap();
        assert_eq!(policy.backend, BackendKind::Auto);
    }

    #[test]
    fn host_channel_policy_preserves_the_exact_loopback_literal() {
        let mut settings = IdentityFixture::new().settings;
        let paths = VmLocalPaths {
            ssh: "/usr/bin/ssh".into(),
            identity_file: "/private/key".into(),
            known_hosts: "/private/known_hosts".into(),
            colima: "/usr/bin/colima".into(),
        };
        for (host, expected) in [("127.0.0.2", "127.0.0.2"), ("[::1]", "::1")] {
            settings.host = Some(host.into());
            let policy = host_policy(
                &settings,
                Path::new("/private/vm-host"),
                &paths,
                Some(31234),
            )
            .unwrap();
            let network = serde_json::to_value(policy.network).unwrap();
            assert_eq!(network["host"], expected);
            assert_eq!(network["proxy_port"], 31234);
        }
    }

    #[tokio::test]
    async fn verifies_remote_identity_files_and_node_version() {
        let fixture = IdentityFixture::new();
        let runner = QueueRunner::new(fixture.outputs());
        let verifier = VmWorkerIdentityVerifier::new(fixture.settings.clone(), runner.clone());
        let identity = verifier.verify(&NeverCancel).await.unwrap();
        assert_eq!(
            identity.manifest_sha256,
            fixture.settings.identity_sha256.clone().unwrap()
        );
        assert_eq!(
            identity.chromium,
            PathBuf::from(fixture.settings.chromium.clone().unwrap())
        );
        assert_eq!(runner.calls(), 8);
    }

    #[tokio::test]
    async fn rejects_remote_installation_tree_mismatches() {
        let fixture = IdentityFixture::new();
        let mut outputs = fixture.outputs();
        let mut trees: Value = serde_json::from_slice(&fixture.installation_probe).unwrap();
        trees["worker"]["sha256"] = json!("0".repeat(64));
        outputs[6] = success(serde_json::to_vec(&trees).unwrap());
        let verifier = VmWorkerIdentityVerifier::new(fixture.settings, QueueRunner::new(outputs));
        let failure = verifier.verify(&NeverCancel).await.unwrap_err();
        assert_eq!(failure.code, "worker_identity");
        assert!(failure.message.contains("installation tree"), "{failure:?}");
    }

    #[tokio::test]
    async fn rejects_remote_digest_permission_and_version_mismatches() {
        let fixture = IdentityFixture::new();
        let mut outputs = fixture.outputs();
        outputs[0] = success(String::from_utf8(fixture.probe.clone()).unwrap().replacen(
            &fixture.settings.identity_sha256.clone().unwrap(),
            &"0".repeat(64),
            1,
        ));
        let runner = QueueRunner::new(outputs);
        let verifier = VmWorkerIdentityVerifier::new(fixture.settings.clone(), runner);
        assert_eq!(
            verifier.verify(&NeverCancel).await.unwrap_err().code,
            "worker_identity"
        );

        let fixture = IdentityFixture::new();
        let mut outputs = fixture.outputs();
        outputs[0] = success(String::from_utf8(fixture.probe.clone()).unwrap().replacen(
            "|644|regular file\n",
            "|666|regular file\n",
            1,
        ));
        let verifier =
            VmWorkerIdentityVerifier::new(fixture.settings.clone(), QueueRunner::new(outputs));
        assert_eq!(
            verifier.verify(&NeverCancel).await.unwrap_err().code,
            "worker_identity"
        );

        let fixture = IdentityFixture::new();
        let mut outputs = fixture.outputs();
        outputs[7] = success(b"v20.0.0\n".to_vec());
        let verifier =
            VmWorkerIdentityVerifier::new(fixture.settings.clone(), QueueRunner::new(outputs));
        assert_eq!(
            verifier.verify(&NeverCancel).await.unwrap_err().code,
            "worker_identity"
        );

        let mut settings = IdentityFixture::new().settings;
        settings.chromium = Some("/opt/saya/chromium".into());
        let runner = QueueRunner::new(IdentityFixture::new().outputs());
        let verifier = VmWorkerIdentityVerifier::new(settings, runner.clone());
        assert_eq!(
            verifier.verify(&NeverCancel).await.unwrap_err().code,
            "worker_identity"
        );
        assert_eq!(runner.calls(), 0);
    }

    #[test]
    fn vm_launch_arguments_keep_chromium_sandbox_enabled() {
        let args = worker_launch_arguments(
            "/tmp/runs/one",
            "/tmp/runs/one/proxy.sock",
            31281,
            "/tmp/runs/one/worker.pid",
            "/opt/saya/node/bin/node",
            "/opt/saya/worker/node_modules/@playwright/mcp/cli.js",
            "/tmp/runs/one/config/config.json",
            "/opt/saya/browsers/chromium_headless_shell-1254/chrome-headless-shell-linux64/chrome-headless-shell",
        );
        for required in [
            "--sandbox",
            "--config",
            "--executable-path",
            "--headless",
            "--isolated",
        ] {
            assert!(args.iter().any(|value| value == required), "{required}");
        }
        for forbidden in [
            "--no-sandbox",
            "--disable-setuid-sandbox",
            "--disable-namespace-sandbox",
            "--remote-debugging-port",
        ] {
            assert!(!args.iter().any(|value| value == forbidden), "{forbidden}");
        }
        assert_eq!(args[1], "/tmp/runs/one/proxy.sock");
        assert_eq!(args[2], "31281");
        assert_eq!(args[3], "/tmp/runs/one/worker.pid");
    }

    #[test]
    fn worker_config_pins_chromium_to_the_run_proxy_without_bypasses() {
        let settings = IdentityFixture::new().settings;
        let bytes =
            worker_config_json(&settings, "/tmp/runs/one", "http://127.0.0.1:40123").unwrap();
        let config: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let options = &config["browser"]["launchOptions"];
        assert_eq!(options["proxy"]["server"], "http://127.0.0.1:40123");
        assert_eq!(options["chromiumSandbox"], true);
        let args = options["args"].as_array().unwrap();
        let args: Vec<&str> = args.iter().filter_map(|value| value.as_str()).collect();
        for required in [
            "--proxy-server=http://127.0.0.1:40123",
            "--proxy-bypass-list=<-loopback>",
            "--disable-quic",
            "--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
            "--webrtc-ip-handling-policy=disable_non_proxied_udp",
            "--host-resolver-rules=EXCLUDE 127.0.0.1, MAP * ~NOTFOUND",
            "--disable-features=DnsOverHttps",
        ] {
            assert!(args.contains(&required), "{required}");
        }
        assert!(!args.iter().any(|value| value.contains("direct")));
    }

    #[test]
    fn launch_script_requires_a_private_network_namespace_and_never_falls_back() {
        for required in [
            "sudo -n unshare -n",
            "ip link set lo up",
            "ip -o route show",
            "socat TCP-LISTEN:",
            "UNIX-CONNECT:",
            "setpriv --reuid=",
            "setsid -w",
            "network=$tool-missing",
            "network=sudo-unavailable",
            "network=loopback-unavailable",
            "network=unexpected-route",
            "network=proxy-socket-missing",
            "network=proxy-start-failed",
            "network=proxy-not-listening",
        ] {
            assert!(LAUNCH_SCRIPT.contains(required), "{required}");
        }
        assert!(!LAUNCH_SCRIPT.contains("exit 0"));
    }

    #[test]
    fn launch_failure_classification_is_explicit_and_never_generalized() {
        for sentinel in [
            "network=sudo-missing",
            "network=unshare-missing",
            "network=socat-missing",
            "network=setpriv-missing",
            "network=ip-missing",
            "network=sudo-unavailable",
            "network=loopback-unavailable",
            "network=unexpected-route",
            "network=proxy-socket-missing",
            "network=proxy-start-failed",
            "network=proxy-not-listening",
            "remote port forwarding failed for listen path /tmp/run/proxy.sock",
        ] {
            let failure = classify_launch_failure(&NeverCancel, sentinel.as_bytes());
            assert_eq!(
                failure.code, "browser_network_unavailable",
                "sentinel {sentinel}"
            );
        }
        let other = classify_launch_failure(&NeverCancel, b"worker crashed for another reason");
        assert_eq!(other.code, "browser_vm_transport");
        let unrelated = classify_launch_failure(&NeverCancel, b"network=unrelated-diagnostic");
        assert_eq!(
            unrelated.code, "browser_vm_transport",
            "only fixed network capability prefixes map to browser_network_unavailable"
        );
        let cancelled = classify_launch_failure(&Cancelled, b"network=sudo-missing");
        assert_eq!(cancelled.code, "cancelled");
    }

    #[tokio::test(start_paused = true)]
    async fn network_factory_keeps_one_fixed_run_deadline_across_broker_restarts() {
        let factory = Arc::new(
            BrowserNetworkFactory::new(
                BrowserNetworkSettings {
                    enabled: true,
                    allowed_origins: vec!["https://example.com".into()],
                    limits: BrokerLimits {
                        active_timeout_ms: 10,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                "run-deadline",
            )
            .unwrap(),
        );
        let first = factory.start(&NeverCancel).await.unwrap();
        first.shutdown().await.unwrap();
        tokio::time::advance(Duration::from_millis(11)).await;
        let failure = match factory.start(&NeverCancel).await {
            Ok(_) => panic!("an expired Run deadline must not allow a restart"),
            Err(failure) => failure,
        };
        assert_eq!(
            failure.0.code, "browser_network_budget",
            "an expired fixed Run deadline is a budget failure, not an isolation failure"
        );
    }

    #[tokio::test]
    async fn network_factory_never_resets_exhausted_connection_attempts() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let factory = Arc::new(
            BrowserNetworkFactory::new(
                BrowserNetworkSettings {
                    enabled: true,
                    allowed_origins: vec!["https://example.com".into()],
                    limits: BrokerLimits {
                        max_total_connections: 1,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                "run-connections",
            )
            .unwrap(),
        );
        let first = factory.start(&NeverCancel).await.unwrap();
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", first.endpoint_port()))
            .await
            .unwrap();
        stream
            .write_all(b"CONNECT denied.example:443 HTTP/1.1\r\nHost: denied.example:443\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response).await;
        assert!(String::from_utf8_lossy(&response).contains("403"));
        first.shutdown().await.unwrap();
        drop(stream);

        let restarted = factory.start(&NeverCancel).await.unwrap();
        let mut denied = tokio::net::TcpStream::connect(("127.0.0.1", restarted.endpoint_port()))
            .await
            .unwrap();
        denied
            .write_all(b"CONNECT denied.example:443 HTTP/1.1\r\nHost: denied.example:443\r\n\r\n")
            .await
            .unwrap();
        let mut second = Vec::new();
        let _ = denied.read_to_end(&mut second).await;
        assert!(
            second.is_empty(),
            "an exhausted Run budget must stay exhausted after a restart: {second:?}"
        );
        restarted.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn network_factory_never_resets_spent_bytes() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let mut request = Vec::new();
                let _ = stream.read_to_end(&mut request).await;
                let _ = stream.write_all(response).await;
            }
        });
        let rewritten = format!(
            "GET / HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            origin.trim_start_matches("http://")
        );
        let capacity = rewritten.len() as u64 + response.len() as u64 + 1;
        let factory = Arc::new(
            BrowserNetworkFactory::new(
                BrowserNetworkSettings {
                    enabled: true,
                    allowed_origins: vec![origin.clone()],
                    localhost_development_origin: Some(origin.clone()),
                    limits: BrokerLimits {
                        max_total_bytes: capacity,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                "run-bytes",
            )
            .unwrap(),
        );
        let send = |endpoint: u16, origin: String| async move {
            let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", endpoint))
                .await
                .unwrap();
            stream
                .write_all(
                    format!(
                        "GET {origin}/ HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
                        origin.trim_start_matches("http://")
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            let mut response = Vec::new();
            let _ = stream.read_to_end(&mut response).await;
            response
        };
        let first = factory.start(&NeverCancel).await.unwrap();
        assert!(
            send(first.endpoint_port(), origin.clone())
                .await
                .starts_with(b"HTTP/1.1 200"),
            "the first request must fit the Run budget"
        );
        first.shutdown().await.unwrap();
        let restarted = factory.start(&NeverCancel).await.unwrap();
        assert!(
            !send(restarted.endpoint_port(), origin)
                .await
                .starts_with(b"HTTP/1.1 200"),
            "spent Run bytes must not reset when the broker restarts"
        );
        restarted.shutdown().await.unwrap();
    }

    #[test]
    fn parses_remote_pid_only_for_the_pinned_worker() {
        let pid = parse_pid(b"pid=4242\ncmdline=/opt/saya/node /opt/saya/worker/cli.js\n").unwrap();
        assert_eq!(pid, 4242);
        assert!(parse_pid(b"pid=missing\n").is_none());
        assert!(parse_pid(b"pid=1\ncmdline=node cli.js\n").is_none());
        assert!(parse_pid(b"pid=42\ncmdline=node cli.js --no-sandbox\n").is_none());
        assert!(parse_pid(b"pid=42\ncmdline=node cli.js --disable-setuid-sandbox\n").is_none());
    }

    #[tokio::test]
    async fn remote_cleanup_requires_explicit_confirmation() {
        let settings = IdentityFixture::new().settings;
        let runner = QueueRunner::new(
            [success(b"worker=alive\nrunroot=gone\n".to_vec())]
                .into_iter()
                .collect(),
        );
        let control = RunnerRemoteControl {
            settings: settings.clone(),
            runner: runner.clone(),
            run_root: "/tmp/saya-runs/one".into(),
            pid_file: "/tmp/saya-runs/one/worker.pid".into(),
            cli_js: "/opt/saya/worker/cli.js".into(),
        };
        let failure = control.terminate(true).await.unwrap_err();
        assert_eq!(failure.code, "outcome_unknown");
        assert!(failure.uncertain);
        assert_eq!(runner.scripts()[0], CLEANUP_SCRIPT);

        let runner = QueueRunner::new(
            [success(b"worker=gone\nrunroot=gone\n".to_vec())]
                .into_iter()
                .collect(),
        );
        let control = RunnerRemoteControl {
            settings,
            runner: QueueRunner::new(std::mem::take(&mut *runner.outputs.lock().unwrap())),
            run_root: "/tmp/saya-runs/one".into(),
            pid_file: "/tmp/saya-runs/one/worker.pid".into(),
            cli_js: "/opt/saya/worker/cli.js".into(),
        };
        control.terminate(true).await.unwrap();
    }

    #[tokio::test]
    async fn vm_sandbox_evidence_comes_from_remote_proc() {
        let settings = IdentityFixture::new().settings;
        let runner = QueueRunner::new(
            [success(
                b"renderer=77\ncmdline=/opt/saya/chromium --type=renderer --no-zygote\nnonewprivs=1\nseccomp=2\n".to_vec(),
            )]
            .into_iter()
            .collect(),
        );
        let inspector = VmChromiumSandboxInspector::new(settings.clone(), runner);
        let evidence = inspector
            .inspect(42, b"InitializeSandbox() called", &NeverCancel)
            .await
            .unwrap();
        assert_eq!(evidence.renderer_pid, 77);
        assert!(evidence.no_new_privs);
        assert_eq!(evidence.seccomp_mode, 2);
        assert!(evidence.sandbox_arguments_valid);
        assert!(evidence.stderr_initializes_sandbox);

        let runner = QueueRunner::new(
            [success(
                b"renderer=77\ncmdline=/opt/saya/chromium --type=renderer --no-sandbox\nnonewprivs=1\nseccomp=2\n".to_vec(),
            )]
            .into_iter()
            .collect(),
        );
        let inspector = VmChromiumSandboxInspector::new(settings, runner);
        let failure = inspector.inspect(42, b"", &NeverCancel).await.unwrap_err();
        assert_eq!(failure.code, "browser_sandbox_unavailable");
    }

    #[tokio::test]
    async fn cancelled_vm_launch_dispatches_no_remote_command() {
        let settings = IdentityFixture::new().settings;
        let runner = QueueRunner::new(VecDeque::new());
        let launcher = VmWorkerLauncher::new(
            settings,
            runner.clone(),
            ExecConfig::default(),
            test_network_factory(),
        );
        let cancel = AlreadyCancelled;
        let request = WorkerLaunchRequest {
            argv: Vec::new(),
            cwd: PathBuf::from("/tmp"),
            env: BTreeMap::new(),
            timeout: Duration::from_secs(1),
            stderr_limit: 4096,
            mcp_limits: McpLimits::default(),
            directories: BrowserSandboxDirectories {
                profile: PathBuf::from("/tmp/p"),
                output: PathBuf::from("/tmp/o"),
                tmp: PathBuf::from("/tmp/t"),
                work: PathBuf::from("/tmp/w"),
                config: PathBuf::from("/tmp/c"),
            },
            execution: None,
            identity: None,
        };
        let failure = match launcher.launch(request, &cancel).await {
            Ok(_) => panic!("cancelled launch must not produce a connection"),
            Err(failure) => failure,
        };
        assert_eq!(failure.code, "cancelled");
        assert_eq!(runner.calls(), 0);
    }

    struct AlreadyCancelled;
    impl Cancellation for AlreadyCancelled {
        fn is_cancelled(&self) -> bool {
            true
        }
        fn cancelled(
            &self,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
            Box::pin(std::future::ready(()))
        }
    }

    #[cfg(target_os = "macos")]
    struct SwitchCancel(Arc<AtomicBool>);
    #[cfg(target_os = "macos")]
    impl Cancellation for SwitchCancel {
        fn is_cancelled(&self) -> bool {
            self.0.load(Ordering::Acquire)
        }
        fn cancelled(
            &self,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
            Box::pin(std::future::pending())
        }
    }

    /// Runner that flips cancellation after it confirms the remote Run root.
    #[cfg(target_os = "macos")]
    struct PrepareThenCancel {
        cancel: Arc<AtomicBool>,
        calls: AtomicUsize,
    }

    #[cfg(target_os = "macos")]
    #[async_trait]
    impl VmCommandRunner for PrepareThenCancel {
        async fn run(
            &self,
            spec: VmCommandSpec<'_>,
            _cancel: &dyn Cancellation,
        ) -> Result<VmCommandOutput, BrowserFailure> {
            let index = self.calls.fetch_add(1, Ordering::AcqRel);
            if index == 0 {
                self.cancel.store(true, Ordering::Release);
                Ok(success(spec.args[0].clone().into_bytes()))
            } else {
                Ok(success(b"worker=absent\nrunroot=gone\n".to_vec()))
            }
        }
    }

    #[cfg(target_os = "macos")]
    struct HangPrepare {
        started: tokio::sync::Notify,
        calls: AtomicUsize,
    }

    #[cfg(target_os = "macos")]
    #[async_trait]
    impl VmCommandRunner for HangPrepare {
        async fn run(
            &self,
            _spec: VmCommandSpec<'_>,
            _cancel: &dyn Cancellation,
        ) -> Result<VmCommandOutput, BrowserFailure> {
            let index = self.calls.fetch_add(1, Ordering::AcqRel);
            if index == 0 {
                self.started.notify_one();
                std::future::pending::<()>().await;
                unreachable!("the hanging prepare never completes");
            }
            Ok(success(b"worker=absent\nrunroot=gone\n".to_vec()))
        }
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn dropping_the_launch_future_keeps_the_remote_cleanup_handle() {
        let fixture = LocalFixture::new();
        let runner = Arc::new(HangPrepare {
            started: tokio::sync::Notify::new(),
            calls: AtomicUsize::new(0),
        });
        let launcher = Arc::new(VmWorkerLauncher::new(
            fixture.settings.clone(),
            runner.clone(),
            ExecConfig::default(),
            test_network_factory(),
        ));
        let workspace = tempfile::tempdir().unwrap();
        let policy =
            host_policy(&fixture.settings, workspace.path(), &fixture.paths, None).unwrap();
        let environment = execution::prepare(&policy).unwrap();
        let directories = BrowserSandboxDirectories {
            profile: workspace.path().join("profile"),
            output: workspace.path().join("output"),
            tmp: workspace.path().join("tmp"),
            work: workspace.path().join("work"),
            config: workspace.path().join("config"),
        };
        for directory in [
            &directories.profile,
            &directories.output,
            &directories.tmp,
            &directories.work,
            &directories.config,
        ] {
            fs::create_dir_all(directory).unwrap();
        }
        let request = WorkerLaunchRequest {
            argv: Vec::new(),
            cwd: directories.work.clone(),
            env: BTreeMap::new(),
            timeout: Duration::from_secs(1),
            stderr_limit: 4096,
            mcp_limits: McpLimits::default(),
            directories,
            execution: Some(environment.clone()),
            identity: Some(VerifiedWorkerIdentity {
                node: PathBuf::from("/opt/saya/node"),
                worker_root: PathBuf::from("/opt/saya/worker"),
                cli_js: PathBuf::from("/opt/saya/worker/node_modules/@playwright/mcp/cli.js"),
                chromium: PathBuf::from(
                    "/opt/saya/browsers/chromium_headless_shell-1254/chrome-headless-shell-linux64/chrome-headless-shell",
                ),
                manifest_sha256: fixture.settings.identity_sha256.clone().unwrap(),
            }),
        };
        let task_launcher = launcher.clone();
        let handle = tokio::spawn(async move {
            let cancel = NeverCancel;
            task_launcher.launch(request, &cancel).await
        });
        tokio::time::timeout(Duration::from_secs(5), runner.started.notified())
            .await
            .expect("the launch must reach the hanging prepare");
        handle.abort();
        let _ = handle.await;
        assert_eq!(
            launcher.pending_entries().len(),
            1,
            "the dropped launch must keep the remote cleanup handle"
        );
        launcher.retry_pending_cleanup().await.unwrap();
        assert!(launcher.pending_entries().is_empty());
        assert_eq!(runner.calls.load(Ordering::Acquire), 2);
        let _ = environment.cleanup();
    }

    #[cfg(target_os = "macos")]
    struct LocalFixture {
        _directory: tempfile::TempDir,
        settings: BrowserVmSettings,
        paths: VmLocalPaths,
    }

    #[cfg(target_os = "macos")]
    impl LocalFixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let identity = directory.path().join("id_ed25519");
            let known_hosts = directory.path().join("known_hosts");
            fs::write(&identity, b"private key placeholder").unwrap();
            fs::set_permissions(&identity, fs::Permissions::from_mode(0o600)).unwrap();
            fs::write(&known_hosts, b"127.0.0.1 ssh-ed25519 AAAA\n").unwrap();
            fs::set_permissions(&known_hosts, fs::Permissions::from_mode(0o644)).unwrap();
            let mut settings = IdentityFixture::new().settings;
            settings.ssh = Some(PathBuf::from("/usr/bin/ssh"));
            settings.colima = Some(PathBuf::from("/usr/bin/ssh"));
            settings.identity_file = Some(identity);
            settings.known_hosts = Some(known_hosts);
            let paths = settings.preflight_local().unwrap();
            Self {
                _directory: directory,
                settings,
                paths,
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn canonical_identity_paths_are_revalidated_after_symlink_resolution() {
        let fixture = LocalFixture::new();
        let root = fixture._directory.path();
        let target = root.join("key\nProxyCommand=evil");
        fs::write(&target, b"private key placeholder").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let link = root.join("identity-link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let mut settings = fixture.settings.clone();
        settings.identity_file = Some(link);
        assert!(
            settings.preflight_local().is_err(),
            "a symlink target containing a newline must not reach ssh_config"
        );

        let known_target = root.join("known hosts\nProxyCommand=evil");
        fs::write(&known_target, b"127.0.0.1 ssh-ed25519 AAAA\n").unwrap();
        fs::set_permissions(&known_target, fs::Permissions::from_mode(0o644)).unwrap();
        let known_link = root.join("known-hosts-link");
        std::os::unix::fs::symlink(&known_target, &known_link).unwrap();
        settings = fixture.settings.clone();
        settings.known_hosts = Some(known_link);
        assert!(settings.preflight_local().is_err());
    }

    #[test]
    fn bracketed_loopback_host_is_normalized_for_config_and_argv() {
        assert_eq!(normalized_loopback_host("[::1]").as_deref(), Some("::1"));
        assert_eq!(
            normalized_loopback_host("127.0.0.1").as_deref(),
            Some("127.0.0.1")
        );
        assert!(normalized_loopback_host("[::2]").is_none());
        assert!(normalized_loopback_host(" [::1]").is_none());

        let mut settings = IdentityFixture::new().settings;
        settings.host = Some("[::1]".into());
        settings.validate().unwrap();
        let paths = VmLocalPaths {
            ssh: PathBuf::from("/usr/bin/ssh"),
            identity_file: PathBuf::from("/private/tmp/key"),
            known_hosts: PathBuf::from("/private/tmp/known_hosts"),
            colima: PathBuf::from("/usr/local/bin/colima"),
        };
        let config = strict_ssh_config(&settings, &paths).unwrap();
        assert!(config.contains("HostName ::1"));
        assert!(!config.contains("[::1]"));
        let argv = ssh_argv(
            &settings,
            &paths,
            Path::new("/private/tmp/ssh_config"),
            PROBE_SCRIPT,
            &[],
            None,
        )
        .unwrap();
        assert!(argv.iter().any(|value| value == "::1"));
        assert!(!argv.iter().any(|value| value.contains("[::1]")));
    }

    #[test]
    fn command_outcomes_distinguish_cancel_before_dispatch_from_unknown() {
        let cancelled = VmCommandOutput {
            status: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
            timed_out: false,
            cancelled: true,
        };
        let before = vm_command_outcome(cancelled.clone(), false, "probe").unwrap_err();
        assert_eq!(before.code, "cancelled");
        assert!(!before.uncertain);
        let after = vm_command_outcome(cancelled, true, "probe").unwrap_err();
        assert_eq!(after.code, "outcome_unknown");
        assert!(after.uncertain);

        let timed_out = VmCommandOutput {
            status: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
            timed_out: true,
            cancelled: false,
        };
        let failure = vm_command_outcome(timed_out, true, "cleanup").unwrap_err();
        assert_eq!(failure.code, "outcome_unknown");
        assert!(failure.uncertain);

        let known = vm_command_result(
            Err(BrowserFailure::new("worker_spawn", "spawn failed")),
            "identity probe",
        )
        .unwrap_err();
        assert_eq!(known.code, "worker_spawn");
        assert!(!known.uncertain, "a pre-dispatch failure stays known");
        let transport = vm_command_result(
            Err(BrowserFailure::uncertain("transport lost after dispatch")),
            "identity probe",
        )
        .unwrap_err();
        assert_eq!(transport.code, "outcome_unknown");
        assert!(transport.uncertain);
    }

    #[tokio::test]
    async fn cancelled_or_timed_out_identity_probe_is_not_an_identity_failure() {
        let fixture = IdentityFixture::new();
        let mut outputs = fixture.outputs();
        outputs[0] = VmCommandOutput {
            status: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
            timed_out: true,
            cancelled: false,
        };
        let runner = QueueRunner::new(outputs);
        let verifier = VmWorkerIdentityVerifier::new(fixture.settings.clone(), runner);
        let failure = verifier.verify(&NeverCancel).await.unwrap_err();
        assert_eq!(failure.code, "outcome_unknown");
        assert!(failure.uncertain);

        let fixture = IdentityFixture::new();
        let mut outputs = fixture.outputs();
        outputs[0] = VmCommandOutput {
            status: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
            timed_out: false,
            cancelled: true,
        };
        let runner = QueueRunner::new(outputs);
        let verifier = VmWorkerIdentityVerifier::new(fixture.settings.clone(), runner);
        let failure = verifier.verify(&NeverCancel).await.unwrap_err();
        assert_eq!(failure.code, "outcome_unknown");
    }

    #[test]
    fn prepare_script_creates_all_launch_directories_with_private_modes() {
        let directory = tempfile::tempdir().unwrap();
        let run_root = directory.path().join("run");
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg(PREPARE_RUN_SCRIPT)
            .arg("saya-browser-vm-cleanup")
            .arg(&run_root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        {
            use std::io::Write as _;
            child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(b"{\"config\":true}")
                .unwrap();
        }
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{:?}", output);
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            run_root.display().to_string()
        );

        let mut expected = vec![
            "profile",
            "output",
            "tmp",
            "work",
            "config",
            "home",
            "home/cache",
            "home/data",
            "home/state",
            "home/runtime",
            "s",
        ];
        // Every XDG/HOME/TMPDIR/socket directory exported by the launch script
        // must already exist under the Run root.
        for line in LAUNCH_SCRIPT.lines() {
            let line = line.trim();
            let Some(value) = line.strip_prefix("export ") else {
                continue;
            };
            let Some((name, path)) = value.split_once('=') else {
                continue;
            };
            if !matches!(name, "HOME" | "TMPDIR" | "PWTEST_SOCKETS_DIR")
                && !name.starts_with("XDG_")
            {
                continue;
            }
            let path = path.trim_matches('"');
            if let Some(relative) = path.strip_prefix("$1/") {
                expected.push(relative);
            }
        }
        expected.sort_unstable();
        expected.dedup();
        for relative in expected {
            let path = run_root.join(relative);
            assert!(path.is_dir(), "missing prepared directory {relative}");
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{relative} mode is {mode:o}");
        }
        assert_eq!(
            fs::read(run_root.join("config/config.json")).unwrap(),
            b"{\"config\":true}"
        );
    }

    struct CountingControl {
        calls: AtomicUsize,
        fail_times: AtomicUsize,
    }

    #[async_trait]
    impl VmRemoteControl for CountingControl {
        async fn terminate(&self, _expect_worker: bool) -> Result<(), BrowserFailure> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            if self.fail_times.load(Ordering::Acquire) > 0 {
                self.fail_times.fetch_sub(1, Ordering::AcqRel);
                return Err(remote_cleanup_failure("test cleanup failure"));
            }
            Ok(())
        }
    }

    fn counting_control(fail_times: usize) -> Arc<CountingControl> {
        Arc::new(CountingControl {
            calls: AtomicUsize::new(0),
            fail_times: AtomicUsize::new(fail_times),
        })
    }

    struct DelayedControl {
        calls: AtomicUsize,
        delay_ms: AtomicU64,
    }

    impl DelayedControl {
        fn new(delay_ms: u64) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                delay_ms: AtomicU64::new(delay_ms),
            })
        }
    }

    #[async_trait]
    impl VmRemoteControl for DelayedControl {
        async fn terminate(&self, _expect_worker: bool) -> Result<(), BrowserFailure> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            let delay = self.delay_ms.load(Ordering::Acquire);
            if delay > 0 {
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            Ok(())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn outer_timeout_keeps_every_pending_cleanup_handle_for_a_later_retry() {
        let settings = IdentityFixture::new().settings;
        let launcher = VmWorkerLauncher::new(
            settings,
            QueueRunner::new(VecDeque::new()),
            ExecConfig::default(),
            test_network_factory(),
        );
        let first = DelayedControl::new(30_000);
        let second = DelayedControl::new(30_000);

        // A ten-second outer timeout drops the cleanup future while the
        // remote command is still in flight. Both the current and the later
        // handle must stay registered.
        let timed = tokio::time::timeout(
            Duration::from_secs(10),
            launcher.cleanup_or_retain(first.clone(), false, remote_failure("first launch failed")),
        )
        .await;
        assert!(timed.is_err(), "the outer timeout must drop the future");
        let timed = tokio::time::timeout(
            Duration::from_secs(10),
            launcher.cleanup_or_retain(
                second.clone(),
                true,
                remote_failure("second launch failed"),
            ),
        )
        .await;
        assert!(timed.is_err(), "the outer timeout must drop the future");

        {
            let entries = launcher.pending_entries();
            assert_eq!(entries.len(), 2, "neither handle may be dropped");
            assert!(!entries[0].expect_worker.load(Ordering::Acquire));
            assert!(entries[1].expect_worker.load(Ordering::Acquire));
        }

        first.delay_ms.store(0, Ordering::Release);
        second.delay_ms.store(0, Ordering::Release);
        launcher.retry_pending_cleanup().await.unwrap();
        assert!(launcher.pending_entries().is_empty());
        assert_eq!(first.calls.load(Ordering::Acquire), 2);
        assert_eq!(second.calls.load(Ordering::Acquire), 2);
    }

    #[test]
    fn drop_cleanup_without_a_tokio_runtime_reports_unknown_not_success() {
        let control = counting_control(0);
        let cleanup = Arc::new(ConnectionCleanup::new(control.clone()));
        assert!(!schedule_drop_cleanup(&cleanup));
        assert_eq!(control.calls.load(Ordering::Acquire), 0);
        assert!(
            cleanup.pending(),
            "without a runtime the cleanup stays unknown"
        );
    }

    #[tokio::test]
    async fn drop_cleanup_spawns_bounded_work_when_a_runtime_exists() {
        let control = counting_control(0);
        let cleanup = Arc::new(ConnectionCleanup::new(control.clone()));
        assert!(schedule_drop_cleanup(&cleanup));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(control.calls.load(Ordering::Acquire), 1);
        assert!(!cleanup.pending());
    }

    #[tokio::test]
    async fn failed_cleanup_stays_pending_until_a_retry_confirms_it() {
        let control = counting_control(1);
        let cleanup = ConnectionCleanup::new(control.clone());
        assert!(cleanup.terminate(true).await.is_err());
        assert!(cleanup.pending());
        cleanup.terminate(true).await.unwrap();
        assert!(!cleanup.pending());
        assert_eq!(control.calls.load(Ordering::Acquire), 2);
    }

    #[tokio::test]
    async fn failed_launch_cleanup_is_retained_by_the_launcher_and_retried() {
        let settings = IdentityFixture::new().settings;
        let runner = QueueRunner::new(
            [
                success(b"worker=alive\nrunroot=gone\n".to_vec()),
                success(b"worker=alive\nrunroot=gone\n".to_vec()),
            ]
            .into_iter()
            .collect(),
        );
        let control: Arc<dyn VmRemoteControl> = Arc::new(RunnerRemoteControl {
            settings: settings.clone(),
            runner: runner.clone(),
            run_root: "/tmp/saya-runs/retained".into(),
            pid_file: "/tmp/saya-runs/retained/worker.pid".into(),
            cli_js: "/opt/saya/worker/cli.js".into(),
        });
        let launcher = VmWorkerLauncher::new(
            settings,
            runner.clone(),
            ExecConfig::default(),
            test_network_factory(),
        );
        let failure = launcher
            .cleanup_or_retain(control, false, remote_failure("prepare failed"))
            .await;
        assert_eq!(failure.code, "outcome_unknown");
        assert!(failure.uncertain);

        let retry = launcher.retry_pending_cleanup().await.unwrap_err();
        assert!(retry.uncertain);
        runner
            .outputs
            .lock()
            .unwrap()
            .push_back(success(b"worker=absent\nrunroot=gone\n".to_vec()));
        launcher.retry_pending_cleanup().await.unwrap();
    }

    #[test]
    fn cleanup_script_treats_exited_workers_as_gone_and_removes_the_run_root() {
        // PHP-style zombie detection is documented in the script: a State of
        // Z counts as gone so a cleanly exited worker does not block removal.
        assert!(CLEANUP_SCRIPT.contains("\"$state\" != \"Z\""));
        assert!(CLEANUP_SCRIPT.contains("group_live"));

        let directory = tempfile::tempdir().unwrap();
        let run_root = directory.path().join("run");
        fs::create_dir(&run_root).unwrap();
        let pid_file = run_root.join("worker.pid");
        fs::write(&pid_file, b"999999").unwrap();
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(CLEANUP_SCRIPT)
            .arg("saya-browser-vm-cleanup")
            .arg(&run_root)
            .arg(&pid_file)
            .arg("/opt/saya/worker/cli.js")
            .arg("0")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{:?} {text}", output);
        assert!(
            text.contains("worker=gone") || text.contains("worker=absent"),
            "{text}"
        );
        assert!(text.contains("runroot=gone"), "{text}");
        assert!(!run_root.exists());
    }

    fn write_fake_proc(proc_root: &Path, pid: u32, command: &str, state: &str, children: &[u32]) {
        let directory = proc_root.join(pid.to_string());
        let task = directory.join("task").join(pid.to_string());
        fs::create_dir_all(&task).unwrap();
        fs::write(directory.join("cmdline"), command).unwrap();
        fs::write(directory.join("status"), format!("State:\t{state}\n")).unwrap();
        let children = children
            .iter()
            .map(|child| format!("{child} "))
            .collect::<String>();
        fs::write(task.join("children"), children).unwrap();
    }

    /// Start a process that is reparented after the launcher shell exits, so
    /// killing it does not leave a zombie owned by the test process.
    fn spawn_orphan_sleep() -> u32 {
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg("sleep 30 >/dev/null 2>&1 & echo $!")
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .unwrap()
    }

    fn pid_is_alive(pid: u32) -> bool {
        std::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn wait_for_pid_gone(pid: u32) -> bool {
        for _ in 0..50 {
            if !pid_is_alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        !pid_is_alive(pid)
    }

    fn run_cleanup_script(
        run_root: &Path,
        pid_file: &Path,
        cli: &str,
        expect: &str,
        proc_root: &Path,
    ) -> std::process::Output {
        std::process::Command::new("sh")
            .arg("-c")
            .arg(CLEANUP_SCRIPT)
            .arg("saya-browser-vm-cleanup")
            .arg(run_root)
            .arg(pid_file)
            .arg(cli)
            .arg(expect)
            .arg(proc_root)
            .output()
            .unwrap()
    }

    #[test]
    fn cleanup_script_scans_when_the_pid_file_is_missing() {
        let directory = tempfile::tempdir().unwrap();
        let run_root = directory.path().join("run");
        fs::create_dir(&run_root).unwrap();
        let proc_root = directory.path().join("proc");
        fs::create_dir(&proc_root).unwrap();
        let pid_file = run_root.join("worker.pid");

        let output = run_cleanup_script(
            &run_root,
            &pid_file,
            "/opt/saya/worker/cli.js",
            "1",
            &proc_root,
        );
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{:?} {text}", output);
        assert!(text.contains("worker=absent"), "{text}");
        assert!(!text.contains("worker=unknown"), "{text}");
        assert!(text.contains("runroot=gone"), "{text}");
        assert!(!run_root.exists());
    }

    #[test]
    fn cleanup_script_terminates_related_processes_when_the_pid_file_is_missing() {
        let directory = tempfile::tempdir().unwrap();
        // Use the production Run-root shape: the worker and Chromium argv
        // contain this path, which also contains the `saya-browser-vm` prefix.
        // The cleanup shell's own label must be distinct so the scan does not
        // exclude the very processes it must terminate.
        let run_root = directory
            .path()
            .join("saya-runs")
            .join("saya-browser-vm-0123456789abcdef");
        fs::create_dir_all(&run_root).unwrap();
        let proc_root = directory.path().join("proc");
        fs::create_dir(&proc_root).unwrap();
        let pid_file = run_root.join("worker.pid");
        assert!(CLEANUP_SCRIPT.contains("*saya-browser-vm-cleanup*"));

        let worker = spawn_orphan_sleep();
        let descendant = spawn_orphan_sleep();
        write_fake_proc(
            &proc_root,
            worker,
            &format!("node {}/config/config.json --sandbox", run_root.display()),
            "S",
            &[descendant],
        );
        write_fake_proc(
            &proc_root,
            descendant,
            &format!("chrome --user-data-dir={}/profile", run_root.display()),
            "S",
            &[],
        );

        let output = run_cleanup_script(
            &run_root,
            &pid_file,
            "/opt/saya/worker/cli.js",
            "1",
            &proc_root,
        );
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{:?} {text}", output);
        assert!(text.contains("worker=gone"), "{text}");
        assert!(text.contains("runroot=gone"), "{text}");
        assert!(!run_root.exists());
        assert!(
            wait_for_pid_gone(worker),
            "scanned worker {worker} survived"
        );
        assert!(
            wait_for_pid_gone(descendant),
            "scanned descendant {descendant} survived"
        );
    }

    #[test]
    fn cleanup_script_removes_the_run_root_after_a_crashed_worker() {
        let directory = tempfile::tempdir().unwrap();
        let run_root = directory.path().join("run");
        fs::create_dir(&run_root).unwrap();
        let proc_root = directory.path().join("proc");
        fs::create_dir(&proc_root).unwrap();

        let crashed = spawn_orphan_sleep();
        let _ = std::process::Command::new("kill")
            .arg("-KILL")
            .arg(crashed.to_string())
            .status();
        assert!(wait_for_pid_gone(crashed));
        let pid_file = run_root.join("worker.pid");
        fs::write(&pid_file, crashed.to_string()).unwrap();

        let output = run_cleanup_script(
            &run_root,
            &pid_file,
            "/opt/saya/worker/cli.js",
            "1",
            &proc_root,
        );
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{:?} {text}", output);
        assert!(text.contains("worker=absent"), "{text}");
        assert!(text.contains("runroot=gone"), "{text}");
        assert!(!run_root.exists());
    }

    #[test]
    fn cleanup_script_terminates_a_live_worker_and_its_descendants_from_the_pid_file() {
        let directory = tempfile::tempdir().unwrap();
        let run_root = directory.path().join("run");
        fs::create_dir(&run_root).unwrap();
        let proc_root = directory.path().join("proc");
        fs::create_dir(&proc_root).unwrap();
        let pid_file = run_root.join("worker.pid");

        let worker = spawn_orphan_sleep();
        let descendant = spawn_orphan_sleep();
        fs::write(&pid_file, worker.to_string()).unwrap();
        write_fake_proc(
            &proc_root,
            worker,
            "node /opt/saya/node /opt/saya/worker/cli.js --sandbox",
            "S",
            &[descendant],
        );
        write_fake_proc(
            &proc_root,
            descendant,
            &format!("chrome --user-data-dir={}/profile", run_root.display()),
            "S",
            &[],
        );

        let output = run_cleanup_script(
            &run_root,
            &pid_file,
            "/opt/saya/worker/cli.js",
            "1",
            &proc_root,
        );
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{:?} {text}", output);
        assert!(text.contains("worker=gone"), "{text}");
        assert!(text.contains("runroot=gone"), "{text}");
        assert!(!run_root.exists());
        assert!(wait_for_pid_gone(worker), "worker {worker} survived");
        assert!(
            wait_for_pid_gone(descendant),
            "descendant {descendant} survived"
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn cancel_after_remote_prepare_cleans_the_run_root() {
        let fixture = LocalFixture::new();
        let cancel = Arc::new(AtomicBool::new(false));
        let runner = Arc::new(PrepareThenCancel {
            cancel: cancel.clone(),
            calls: AtomicUsize::new(0),
        });
        let launcher = VmWorkerLauncher::new(
            fixture.settings.clone(),
            runner.clone(),
            ExecConfig::default(),
            test_network_factory(),
        );
        let workspace = tempfile::tempdir().unwrap();
        let policy =
            host_policy(&fixture.settings, workspace.path(), &fixture.paths, None).unwrap();
        let environment = execution::prepare(&policy).unwrap();
        let directories = BrowserSandboxDirectories {
            profile: workspace.path().join("profile"),
            output: workspace.path().join("output"),
            tmp: workspace.path().join("tmp"),
            work: workspace.path().join("work"),
            config: workspace.path().join("config"),
        };
        for directory in [
            &directories.profile,
            &directories.output,
            &directories.tmp,
            &directories.work,
            &directories.config,
        ] {
            fs::create_dir_all(directory).unwrap();
        }
        let request = WorkerLaunchRequest {
            argv: Vec::new(),
            cwd: directories.work.clone(),
            env: BTreeMap::new(),
            timeout: Duration::from_secs(1),
            stderr_limit: 4096,
            mcp_limits: McpLimits::default(),
            directories,
            execution: Some(environment.clone()),
            identity: Some(VerifiedWorkerIdentity {
                node: PathBuf::from("/opt/saya/node"),
                worker_root: PathBuf::from("/opt/saya/worker"),
                cli_js: PathBuf::from("/opt/saya/worker/node_modules/@playwright/mcp/cli.js"),
                chromium: PathBuf::from(
                    "/opt/saya/browsers/chromium_headless_shell-1254/chrome-headless-shell-linux64/chrome-headless-shell",
                ),
                manifest_sha256: fixture.settings.identity_sha256.clone().unwrap(),
            }),
        };
        let cancel_token = SwitchCancel(cancel);
        let failure = match launcher.launch(request, &cancel_token).await {
            Ok(_) => panic!("cancelled launch must not produce a connection"),
            Err(failure) => failure,
        };
        assert_eq!(failure.code, "cancelled");
        assert_eq!(runner.calls.load(Ordering::Acquire), 2);
        let _ = environment.cleanup();
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn vm_factory_creates_and_releases_fresh_host_environments() {
        let fixture = LocalFixture::new();
        let factory = VmBrowserEnvironmentFactory::new(fixture.settings.clone()).unwrap();
        let first = factory.prepare().unwrap();
        let first_directories = first.directories().clone();
        assert!(first_directories.work.is_dir());
        let second = factory.prepare().unwrap();
        let second_directories = second.directories().clone();
        assert_ne!(first_directories.work, second_directories.work);
        assert!(second_directories.work.is_dir());
        first.release().await.unwrap();
        second.release().await.unwrap();
        assert!(!first_directories.work.exists());
        assert!(!second_directories.work.exists());
    }
}
