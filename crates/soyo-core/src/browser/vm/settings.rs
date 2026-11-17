use super::*;

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

pub(super) fn invalid_config(message: &'static str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: "invalid_config".into(),
            message: message.into(),
        },
        None,
        None,
    )
}

pub(super) fn vm_unavailable(message: &'static str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: "browser_vm_unavailable".into(),
            message: message.into(),
        },
        None,
        None,
    )
}

pub(super) fn identity_failure(message: impl Into<String>) -> BrowserFailure {
    BrowserFailure::new("worker_identity", message)
}

pub(super) fn remote_failure(message: impl Into<String>) -> BrowserFailure {
    BrowserFailure::new("browser_vm_transport", message)
}

pub(super) fn remote_cleanup_failure(message: impl Into<String>) -> BrowserFailure {
    BrowserFailure::uncertain(message)
}

pub(super) fn safe_token(value: &str, max: usize) -> bool {
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
pub(super) fn normalized_loopback_host(value: &str) -> Option<String> {
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

    pub(super) fn resolved_remote_paths(&self) -> Result<RemotePaths, BoundaryError> {
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
pub(super) struct RemotePaths {
    pub(super) node: String,
    pub(super) worker_root: String,
    pub(super) chromium: String,
    pub(super) identity_manifest: String,
    pub(super) identity_sha256: String,
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

pub(super) fn validate_ssh_paths(paths: &VmLocalPaths) -> Result<(), BoundaryError> {
    for path in [&paths.ssh, &paths.identity_file, &paths.known_hosts] {
        require_safe_canonical_path(
            path,
            "Browser VM SSH inputs must not contain whitespace, control or quoting characters",
        )?;
    }
    Ok(())
}
