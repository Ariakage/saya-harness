//! Native execution environments shared by commands and bounded file workers.
//!
//! Authorization and durable intent remain the dispatcher's responsibility.
//! Preparing an environment never runs the caller's payload. Callers supervise
//! every launched process, then call `cleanup` after all calls have stopped.

use crate::contract::{BoundaryError, ErrorInfo};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs, io,
    os::unix::{ffi::OsStrExt, fs::DirBuilderExt, process::CommandExt},
    path::{Component, Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

mod cgroup;
mod fds;
mod transport;
mod worker;
pub use transport::{pin_launcher_image, pin_launcher_image_from};
pub use worker::{
    FileWorkerRequest, FileWorkerResponse, WORKER_RPC_MAX_BYTES, execute_file_worker,
    internal_command,
};
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod seatbelt;

/// Backend selection is trusted startup configuration, never a model argument.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    #[default]
    Auto,
    Seatbelt,
    Bubblewrap,
    /// Explicit compatibility mode, reporting every isolation capability absent.
    Host,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityLevel {
    Hard,
    BestEffort,
    Unsupported,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendCapabilities {
    pub filesystem: CapabilityLevel,
    pub network: CapabilityLevel,
    pub syscall_filter: CapabilityLevel,
    pub process_cleanup: CapabilityLevel,
    pub per_process_resources: CapabilityLevel,
    pub total_memory: CapabilityLevel,
    pub total_processes: CapabilityLevel,
    pub cpu_bandwidth: CapabilityLevel,
}
/// cgroup CPU bandwidth, measured in microseconds per period.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CpuQuota {
    pub quota_micros: u64,
    pub period_micros: u64,
}
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResourceLimits {
    pub cpu_seconds: Option<u64>,
    pub file_size_bytes: Option<u64>,
    pub open_files: Option<u64>,
    pub total_memory_bytes: Option<u64>,
    pub total_processes: Option<u64>,
    pub cpu_quota: Option<CpuQuota>,
}
impl ResourceLimits {
    /// Validate numeric limits without preparing a backend or creating artifacts.
    pub fn validate(&self) -> Result<(), BoundaryError> {
        validate_resources(self)
    }
}
/// A proxy's host TCP listener and the private bridge socket needed on Linux.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyEndpoint {
    pub endpoint: std::net::SocketAddr,
    pub unix_socket: Option<PathBuf>,
}
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkPolicy {
    #[default]
    Deny,
    Proxy(ProxyEndpoint),
    /// Allow outbound TCP only to a fixed loopback port. This purpose exists for
    /// the host side of the dedicated VM browser backend: the only reachable
    /// service is the VM's forwarded SSH port, never general browser egress.
    /// Generic policies keep `Deny` as their default.
    LoopbackPort {
        port: u16,
    },
}
/// Trusted host selection for a browser worker environment. It is never a
/// model argument and never binds the workspace, a home directory, the Saya
/// configuration tree or credential directories by default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserSandboxProfile {
    /// Existing Node, Playwright and Chromium installation roots. Each is
    /// mounted read-only; the run-private profile, output and tmp directories
    /// are created below the environment scratch directory instead.
    pub install_roots: Vec<PathBuf>,
}
/// Run-private browser directories. The browser worker may write only here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserSandboxDirectories {
    pub profile: PathBuf,
    pub output: PathBuf,
    pub tmp: PathBuf,
    pub work: PathBuf,
    pub config: PathBuf,
}
/// What the host execution layer actually established for a browser worker.
/// Chromium's own sandbox is verified by the provider, not inferred here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserEnvironmentEvidence {
    pub backend: BackendKind,
    pub nested_user_namespace: bool,
    pub procfs: bool,
    pub dev_shm: bool,
    pub chromium_sandbox_verified_by_provider: bool,
}
#[derive(Debug, Clone)]
pub struct SandboxPolicy {
    pub backend: BackendKind,
    pub workspace: PathBuf,
    pub read_roots: Vec<PathBuf>,
    pub write_roots: Vec<PathBuf>,
    /// Exact files exposed read-only in addition to `read_roots`. Used by the
    /// VM host channel so an SSH identity file and known-hosts file are
    /// readable without exposing their whole parent directory.
    pub read_files: Vec<PathBuf>,
    /// Literal denied paths. Unsupported masks fail preparation.
    pub protected_paths: Vec<PathBuf>,
    pub network: NetworkPolicy,
    pub resources: ResourceLimits,
    /// An explicitly delegated cgroup v2 subtree. No global tree is modified.
    pub cgroup_root: Option<PathBuf>,
    /// Browser workers use a separate filesystem and syscall profile. Generic
    /// policies leave this unset and keep the existing behavior unchanged.
    pub browser: Option<BrowserSandboxProfile>,
}
impl SandboxPolicy {
    pub fn workspace(workspace: PathBuf) -> Self {
        Self {
            backend: BackendKind::Auto,
            read_roots: vec![workspace.clone()],
            write_roots: vec![workspace.clone()],
            read_files: Vec::new(),
            workspace,
            protected_paths: Vec::new(),
            network: NetworkPolicy::Deny,
            resources: ResourceLimits::default(),
            cgroup_root: None,
            browser: None,
        }
    }
    /// Browser-specific policy. Only `install_roots` are exposed read-only;
    /// writable roots always come from the environment's private scratch.
    pub fn browser(profile: BrowserSandboxProfile) -> Self {
        Self {
            backend: BackendKind::Auto,
            // The generic workspace is not bound in browser mode. A temporary
            // directory only satisfies shared policy normalization.
            workspace: std::env::temp_dir(),
            read_roots: Vec::new(),
            write_roots: Vec::new(),
            read_files: Vec::new(),
            protected_paths: Vec::new(),
            network: NetworkPolicy::Deny,
            resources: ResourceLimits::default(),
            cgroup_root: None,
            browser: Some(profile),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentIdentity {
    pub id: String,
    pub backend: BackendKind,
    pub host_os: String,
    pub execution_os: String,
    pub architecture: String,
    pub policy_digest: String,
    /// Present only for browser environments. Generic environments omit it so
    /// their identity report remains byte-for-byte compatible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<BrowserEnvironmentEvidence>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupReport {
    /// Owned artifacts and optional cgroup were removed. Process supervisors
    /// must separately confirm their own child/process-group outcomes.
    pub complete: bool,
}
/// Shared environment lease. Keep it alive while any prepared command runs.
#[derive(Clone)]
pub struct PreparedEnvironment {
    inner: Arc<Environment>,
}
impl std::fmt::Debug for PreparedEnvironment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedEnvironment")
            .field("identity", &self.inner.identity)
            .finish()
    }
}
struct Environment {
    policy: SandboxPolicy,
    identity: EnvironmentIdentity,
    capabilities: BackendCapabilities,
    scratch: PathBuf,
    browser_directories: Option<BrowserSandboxDirectories>,
    cgroup: Option<cgroup::Cgroup>,
    closed: AtomicBool,
    cleanup_lock: Mutex<()>,
    host_worker: Mutex<Option<PathBuf>>,
    active_processes: AtomicUsize,
    #[cfg(target_os = "linux")]
    linux: Option<linux::LinuxBackend>,
    #[cfg(target_os = "macos")]
    seatbelt: Option<seatbelt::SeatbeltBackend>,
}
/// A launcher with owned isolation resources. `configure` composes with an
/// existing supervisor's stdio, process-group and cwd-descriptor callbacks.
pub struct PreparedCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub env: BTreeMap<OsString, OsString>,
    environment: Arc<Environment>,
    bootstrap_index: Option<usize>,
    configured: AtomicBool,
    #[cfg(target_os = "linux")]
    seccomp: Option<Arc<fs::File>>,
    #[cfg(target_os = "linux")]
    mount_fds: Vec<Arc<fs::File>>,
}
/// Future container/VM implementations can produce the same environment lease.
pub trait ExecutionBackend: Send + Sync {
    fn prepare(&self, policy: &SandboxPolicy) -> Result<PreparedEnvironment, BoundaryError>;
}
pub struct NativeBackend;
impl ExecutionBackend for NativeBackend {
    fn prepare(&self, policy: &SandboxPolicy) -> Result<PreparedEnvironment, BoundaryError> {
        prepare(policy)
    }
}

pub fn prepare(policy: &SandboxPolicy) -> Result<PreparedEnvironment, BoundaryError> {
    let mut policy = policy.clone();
    policy.workspace = existing_directory(&policy.workspace)?;
    policy.read_roots = normalize_roots(&policy.read_roots)?;
    policy.write_roots = normalize_roots(&policy.write_roots)?;
    policy.read_files = policy
        .read_files
        .iter()
        .map(|path| existing_file(path))
        .collect::<Result<Vec<_>, _>>()?;
    policy.protected_paths = policy
        .protected_paths
        .iter()
        .map(|path| normalize_path(path))
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(profile) = &mut policy.browser {
        // Browser mode never inherits generic roots: the installer decides
        // exactly which installation trees and private directories exist.
        if !policy.read_roots.is_empty()
            || !policy.write_roots.is_empty()
            || !policy.protected_paths.is_empty()
        {
            return Err(failure(
                "invalid_sandbox_policy",
                "browser sandbox binds only explicit installation roots and private directories",
            ));
        }
        if profile.install_roots.is_empty() || profile.install_roots.len() > 64 {
            return Err(failure(
                "invalid_sandbox_policy",
                "browser sandbox requires between 1 and 64 installation roots",
            ));
        }
        profile.install_roots = profile
            .install_roots
            .iter()
            .map(|path| {
                existing_directory(path).map_err(|_| {
                    failure(
                        "invalid_sandbox_policy",
                        format!(
                            "browser installation root is unavailable: {}",
                            path.display()
                        ),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        profile.install_roots.sort();
        profile.install_roots.dedup();
        // SAFETY: geteuid takes no arguments and has no side effects.
        let root_home = unsafe { libc::geteuid() } == 0;
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .or_else(|| root_home.then(|| PathBuf::from("/root")))
            .and_then(|path| fs::canonicalize(path).ok());
        for path in &profile.install_roots {
            if path == Path::new("/") {
                return Err(failure(
                    "unsupported_fs_policy",
                    "browser installation roots must be narrower than the filesystem root",
                ));
            }
            if home.as_ref().is_some_and(|home| path == home) {
                return Err(failure(
                    "invalid_sandbox_policy",
                    "browser installation roots cannot be the whole home directory",
                ));
            }
            if path
                .components()
                .any(|component| component.as_os_str() == ".saya")
            {
                return Err(failure(
                    "invalid_sandbox_policy",
                    "browser installation roots cannot expose the Saya configuration tree",
                ));
            }
        }
    }
    if let NetworkPolicy::Proxy(proxy) = &mut policy.network {
        if !matches!(proxy.endpoint.ip(), std::net::IpAddr::V4(ip) if ip == std::net::Ipv4Addr::LOCALHOST)
            && !matches!(proxy.endpoint.ip(), std::net::IpAddr::V6(ip) if ip == std::net::Ipv6Addr::LOCALHOST)
            || proxy.endpoint.port() == 0
        {
            return Err(failure(
                "invalid_sandbox_policy",
                "proxy endpoint must be a nonzero loopback address",
            ));
        }
        if let Some(path) = &proxy.unix_socket {
            proxy.unix_socket = Some(normalize_path(path)?);
        }
    }
    if let NetworkPolicy::LoopbackPort { port } = policy.network
        && port == 0
    {
        return Err(failure(
            "invalid_sandbox_policy",
            "loopback TCP policy requires a nonzero port",
        ));
    }
    validate_resources(&policy.resources)?;
    let backend = match policy.backend {
        BackendKind::Auto if cfg!(target_os = "macos") => BackendKind::Seatbelt,
        BackendKind::Auto if cfg!(target_os = "linux") => BackendKind::Bubblewrap,
        BackendKind::Auto => {
            return Err(unavailable(
                "native sandbox backend is unavailable on this platform",
            ));
        }
        backend => backend,
    };
    if let NetworkPolicy::LoopbackPort { .. } = policy.network
        && backend != BackendKind::Seatbelt
    {
        return Err(unavailable(
            "loopback TCP network policy is only implemented by the Seatbelt backend",
        ));
    }
    if policy.browser.is_some() {
        match backend {
            BackendKind::Host => {
                return Err(browser_unavailable(
                    "host execution cannot enforce browser filesystem, namespace or syscall isolation",
                ));
            }
            BackendKind::Seatbelt => {
                #[cfg(target_os = "macos")]
                return Err(browser_unavailable(
                    "macOS Seatbelt prevents Chromium's sandbox_apply from reinitializing; a browser sandbox would require disabling Chromium's own sandbox, which is unsupported",
                ));
                #[cfg(not(target_os = "macos"))]
                return Err(browser_unavailable(
                    "selected browser backend is unavailable on this platform",
                ));
            }
            BackendKind::Bubblewrap => {
                #[cfg(not(target_os = "linux"))]
                return Err(browser_unavailable(
                    "Bubblewrap browser backend is unavailable on this platform",
                ));
            }
            BackendKind::Auto => unreachable!("auto backend was resolved above"),
        }
    }
    if backend != BackendKind::Host
        && policy
            .read_roots
            .iter()
            .chain(&policy.write_roots)
            .any(|path| path == Path::new("/"))
    {
        return Err(failure(
            "unsupported_fs_policy",
            "native sandbox roots must be narrower than the filesystem root",
        ));
    }
    policy.backend = backend;
    let browser = policy.browser.is_some();
    let base = [std::env::temp_dir(), PathBuf::from("/var/tmp")].into_iter().filter_map(|path| fs::canonicalize(path).ok()).find(|path| {
        (browser || !path.starts_with(&policy.workspace)) && !policy.write_roots.iter().any(|root| path.starts_with(root))
    }).ok_or_else(|| failure("unsupported_fs_policy", "no private runtime directory is available outside the workspace and writable roots"))?;
    let scratch = base.join(format!("saya-sandbox-{}", uuid::Uuid::now_v7()));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&scratch)
        .map_err(io_error)?;
    let result = prepare_inner(policy, scratch.clone());
    if result.is_err() {
        let _ = fs::remove_dir_all(scratch);
    }
    result
}
fn prepare_inner(
    policy: SandboxPolicy,
    scratch: PathBuf,
) -> Result<PreparedEnvironment, BoundaryError> {
    fs::create_dir(scratch.join("tmp")).map_err(io_error)?;
    if policy
        .write_roots
        .iter()
        .any(|root| scratch.starts_with(root))
    {
        return Err(failure(
            "unsupported_fs_policy",
            "sandbox runtime directory is inside a writable root",
        ));
    }
    if policy
        .protected_paths
        .iter()
        .any(|path| policy.workspace.starts_with(path) || scratch.starts_with(path))
    {
        return Err(failure(
            "unsupported_fs_policy",
            "workspace or private runtime cannot be inside a protected path",
        ));
    }
    let browser_directories = if policy.browser.is_some() {
        let directories = BrowserSandboxDirectories {
            profile: scratch.join("browser-profile"),
            output: scratch.join("browser-output"),
            tmp: scratch.join("browser-tmp"),
            work: scratch.join("browser-work"),
            config: scratch.join("browser-config"),
        };
        for path in [
            &directories.profile,
            &directories.output,
            &directories.tmp,
            &directories.work,
            &directories.config,
        ] {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(path)
                .map_err(io_error)?;
        }
        for name in ["config", "cache", "data", "state", "runtime"] {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(directories.profile.join(name))
                .map_err(io_error)?;
        }
        Some(directories)
    } else {
        None
    };
    let mut capabilities = BackendCapabilities {
        filesystem: CapabilityLevel::Hard,
        network: CapabilityLevel::Hard,
        syscall_filter: CapabilityLevel::Unsupported,
        process_cleanup: CapabilityLevel::BestEffort,
        per_process_resources: CapabilityLevel::Hard,
        total_memory: CapabilityLevel::Unsupported,
        total_processes: CapabilityLevel::Unsupported,
        cpu_bandwidth: CapabilityLevel::Unsupported,
    };
    #[cfg(target_os = "linux")]
    let linux = if policy.backend == BackendKind::Bubblewrap {
        capabilities.syscall_filter = CapabilityLevel::Hard;
        capabilities.process_cleanup = CapabilityLevel::Hard;
        Some(match (&policy.browser, &browser_directories) {
            (Some(profile), Some(directories)) => {
                linux::LinuxBackend::prepare_browser(&policy, profile, directories, &scratch)?
            }
            _ => linux::LinuxBackend::prepare(&policy, &scratch)?,
        })
    } else {
        None
    };
    #[cfg(target_os = "macos")]
    let seatbelt = if policy.backend == BackendKind::Seatbelt {
        Some(seatbelt::SeatbeltBackend::prepare(&policy, &scratch)?)
    } else {
        None
    };
    if policy.backend == BackendKind::Host {
        capabilities.filesystem = CapabilityLevel::Unsupported;
        capabilities.network = CapabilityLevel::Unsupported;
        capabilities.process_cleanup = CapabilityLevel::BestEffort;
        if !policy.protected_paths.is_empty() || !matches!(policy.network, NetworkPolicy::Deny) {
            return Err(unavailable(
                "host execution cannot enforce protected paths or proxy isolation",
            ));
        }
    } else if (policy.backend == BackendKind::Seatbelt && !cfg!(target_os = "macos"))
        || (policy.backend == BackendKind::Bubblewrap && !cfg!(target_os = "linux"))
    {
        return Err(unavailable(
            "selected backend is unavailable on this platform",
        ));
    }
    let cgroup = cgroup::Cgroup::prepare(
        policy.backend,
        policy.cgroup_root.as_deref(),
        &policy.resources,
        &policy.write_roots,
    )?;
    if cgroup.is_some() {
        capabilities.total_memory = CapabilityLevel::Hard;
        capabilities.total_processes = CapabilityLevel::Hard;
        capabilities.cpu_bandwidth = CapabilityLevel::Hard;
    }
    let digest = Sha256::digest(format!("{policy:?}").as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let browser_evidence = policy.browser.as_ref().map(|_| BrowserEnvironmentEvidence {
        backend: policy.backend,
        nested_user_namespace: true,
        procfs: true,
        dev_shm: true,
        // The provider must verify the real Chromium process. Preparation only
        // establishes the host-side capability envelope.
        chromium_sandbox_verified_by_provider: false,
    });
    let identity = EnvironmentIdentity {
        id: uuid::Uuid::now_v7().to_string(),
        backend: policy.backend,
        host_os: std::env::consts::OS.into(),
        execution_os: std::env::consts::OS.into(),
        architecture: std::env::consts::ARCH.into(),
        policy_digest: digest,
        browser: browser_evidence,
    };
    Ok(PreparedEnvironment {
        inner: Arc::new(Environment {
            policy,
            identity,
            capabilities,
            scratch,
            browser_directories,
            cgroup,
            closed: AtomicBool::new(false),
            cleanup_lock: Mutex::new(()),
            host_worker: Mutex::new(None),
            active_processes: AtomicUsize::new(0),
            #[cfg(target_os = "linux")]
            linux,
            #[cfg(target_os = "macos")]
            seatbelt,
        }),
    })
}
impl PreparedEnvironment {
    pub fn runtime_directory(&self) -> &Path {
        &self.inner.scratch
    }
    pub fn active_processes(&self) -> usize {
        self.inner.active_processes.load(Ordering::Acquire)
    }
    /// Await already-cancelled owners before releasing proxy ports or artifacts.
    /// An unconfirmed abort is retained as `sandbox_cleanup_unknown`.
    pub async fn wait_for_process_cleanup(&self) -> Result<(), BoundaryError> {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while self.active_processes() != 0 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| {
            failure(
                "sandbox_cleanup_unknown",
                "owned process cleanup could not be confirmed",
            )
        })
    }
    pub(crate) fn process_lease(&self) -> Result<ProcessLease, BoundaryError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(failure(
                "sandbox_environment_closed",
                "sandbox environment has been closed",
            ));
        }
        self.inner.active_processes.fetch_add(1, Ordering::AcqRel);
        if self.inner.closed.load(Ordering::Acquire) {
            self.inner.active_processes.fetch_sub(1, Ordering::AcqRel);
            return Err(failure(
                "sandbox_environment_closed",
                "sandbox environment has been closed",
            ));
        }
        Ok(ProcessLease {
            environment: self.clone(),
            completed: false,
        })
    }
    pub fn identity(&self) -> &EnvironmentIdentity {
        &self.inner.identity
    }
    pub fn capabilities(&self) -> &BackendCapabilities {
        &self.inner.capabilities
    }
    /// Private writable directories for a browser worker, if this is one.
    pub fn browser_directories(&self) -> Option<&BrowserSandboxDirectories> {
        self.inner.browser_directories.as_ref()
    }
    /// Host-side browser capability evidence. `None` for generic environments.
    pub fn browser_evidence(&self) -> Option<&BrowserEnvironmentEvidence> {
        self.inner.identity.browser.as_ref()
    }
    pub fn is_browser(&self) -> bool {
        self.inner.policy.browser.is_some()
    }
    /// Trusted built-in file worker in this environment's actual filesystem.
    /// Callers must not guess a GUI or test-harness current executable.
    pub fn file_worker_argv(&self) -> Result<Vec<OsString>, BoundaryError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(failure(
                "sandbox_environment_closed",
                "sandbox environment has been closed",
            ));
        }
        #[cfg(target_os = "linux")]
        if let Some(backend) = &self.inner.linux {
            return Ok(vec![
                backend.helper_path().as_os_str().to_owned(),
                "__saya-file-worker".into(),
            ]);
        }
        #[cfg(target_os = "macos")]
        if let Some(backend) = &self.inner.seatbelt {
            return Ok(vec![
                backend.helper_path().as_os_str().to_owned(),
                "__saya-file-worker".into(),
            ]);
        }
        let mut worker = self
            .inner
            .host_worker
            .lock()
            .map_err(|_| failure("sandbox_prepare_failed", "worker image lock is poisoned"))?;
        if worker.is_none() {
            let image = self.inner.scratch.join("saya-file-worker");
            transport::copy_launcher(&image).map_err(io_error)?;
            *worker = Some(image);
        }
        let image = worker
            .as_ref()
            .ok_or_else(|| failure("sandbox_prepare_failed", "worker image is unavailable"))?;
        Ok(vec![
            image.as_os_str().to_owned(),
            "__saya-file-worker".into(),
        ])
    }
    /// Apply immutable environment values selected by this policy. Call before
    /// hashing the effective environment for an authorization decision.
    pub fn apply_environment(
        &self,
        env: &mut BTreeMap<OsString, OsString>,
    ) -> Result<(), BoundaryError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(failure(
                "sandbox_environment_closed",
                "sandbox environment has been closed",
            ));
        }
        if self.inner.policy.backend == BackendKind::Host {
            return Ok(());
        }
        if let Some(browser) = &self.inner.browser_directories {
            env.insert("TMPDIR".into(), browser.tmp.clone().into_os_string());
            env.insert("HOME".into(), browser.profile.clone().into_os_string());
            for (name, directory) in [
                ("XDG_CONFIG_HOME", "config"),
                ("XDG_CACHE_HOME", "cache"),
                ("XDG_DATA_HOME", "data"),
                ("XDG_STATE_HOME", "state"),
                ("XDG_RUNTIME_DIR", "runtime"),
            ] {
                env.insert(
                    name.into(),
                    browser.profile.join(directory).into_os_string(),
                );
            }
        } else {
            let temporary = if self.inner.policy.backend == BackendKind::Bubblewrap {
                PathBuf::from("/tmp")
            } else {
                self.inner.scratch.join("tmp")
            };
            env.insert("TMPDIR".into(), temporary.into_os_string());
        }
        if let NetworkPolicy::Proxy(proxy) = &self.inner.policy.network {
            let url = OsString::from(format!("http://{}/", proxy.endpoint));
            for name in [
                "HTTP_PROXY",
                "HTTPS_PROXY",
                "ALL_PROXY",
                "http_proxy",
                "https_proxy",
                "all_proxy",
            ] {
                env.insert(name.into(), url.clone());
            }
            for name in ["NO_PROXY", "no_proxy"] {
                env.insert(name.into(), OsString::new());
            }
        }
        Ok(())
    }
    pub fn prepare_command(
        &self,
        argv: &[OsString],
        cwd: &Path,
        env: &BTreeMap<OsString, OsString>,
    ) -> Result<PreparedCommand, BoundaryError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(failure(
                "sandbox_environment_closed",
                "sandbox environment has been closed",
            ));
        }
        if argv.is_empty()
            || argv[0].is_empty()
            || argv.iter().any(|value| value.as_bytes().contains(&0))
        {
            return Err(failure(
                "invalid_args",
                "command requires a nonempty argv without NUL",
            ));
        }
        let cwd = existing_directory(cwd)?;
        let browser_cwd = self
            .inner
            .browser_directories
            .as_ref()
            .is_some_and(|directories| {
                [
                    &directories.profile,
                    &directories.output,
                    &directories.tmp,
                    &directories.work,
                    &directories.config,
                ]
                .into_iter()
                .any(|root| cwd.starts_with(root))
            })
            || self.inner.policy.browser.as_ref().is_some_and(|profile| {
                profile
                    .install_roots
                    .iter()
                    .any(|root| cwd.starts_with(root))
            });
        if self.inner.policy.backend != BackendKind::Host
            && !browser_cwd
            && !self
                .inner
                .policy
                .read_roots
                .iter()
                .chain(&self.inner.policy.write_roots)
                .any(|root| cwd.starts_with(root))
        {
            return Err(failure(
                "path_denied",
                "command cwd is outside sandbox roots",
            ));
        }
        if self
            .inner
            .policy
            .protected_paths
            .iter()
            .any(|path| cwd.starts_with(path))
        {
            return Err(failure("path_denied", "command cwd is protected"));
        }
        let mut command = PreparedCommand {
            program: PathBuf::from(&argv[0]),
            args: argv[1..].to_vec(),
            cwd,
            env: env.clone(),
            environment: self.inner.clone(),
            bootstrap_index: None,
            configured: AtomicBool::new(false),
            #[cfg(target_os = "linux")]
            seccomp: None,
            #[cfg(target_os = "linux")]
            mount_fds: Vec::new(),
        };
        self.apply_environment(&mut command.env)?;
        #[cfg(target_os = "macos")]
        if let Some(backend) = &self.inner.seatbelt {
            backend.wrap(&mut command, argv)?;
        }
        #[cfg(target_os = "linux")]
        if let Some(backend) = &self.inner.linux {
            backend.wrap(&mut command, argv)?;
        }
        Ok(command)
    }
    /// Call only after dispatch has stopped and all owned children are reaped.
    pub fn cleanup(&self) -> Result<CleanupReport, BoundaryError> {
        let _cleanup =
            self.inner.cleanup_lock.lock().map_err(|_| {
                failure("sandbox_cleanup_failed", "sandbox cleanup lock is poisoned")
            })?;
        self.inner.closed.store(true, Ordering::Release);
        if self.active_processes() != 0 {
            return Err(failure(
                "sandbox_cleanup_unknown",
                "owned processes must be reaped before environment cleanup",
            ));
        }
        if let Some(group) = &self.inner.cgroup {
            group.cleanup()?;
        }
        match fs::remove_dir_all(&self.inner.scratch) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(error)),
        }
        Ok(CleanupReport { complete: true })
    }
}
impl PreparedCommand {
    /// Configure a trusted launcher. Does not spawn or authorize the payload.
    pub fn configure(&self, command: &mut Command) -> io::Result<()> {
        if self.environment.closed.load(Ordering::Acquire) {
            return Err(io::Error::from_raw_os_error(libc::ECANCELED));
        }
        if self.configured.swap(true, Ordering::AcqRel) {
            return Err(io::Error::from_raw_os_error(libc::EALREADY));
        }
        let mut args = self.args.clone();
        let environment_descriptor = if let Some(index) = self.bootstrap_index {
            let descriptor = transport::environment_file(&self.env, &self.environment.scratch)?;
            use std::os::fd::AsRawFd;
            args.splice(
                index..index,
                [
                    OsString::from("--env-fd"),
                    descriptor.as_raw_fd().to_string().into(),
                ],
            );
            Some(descriptor)
        } else {
            None
        };
        command.args(args).current_dir(&self.cwd).env_clear();
        if self.bootstrap_index.is_some() {
            let temporary = self
                .environment
                .browser_directories
                .as_ref()
                .map(|directories| directories.tmp.clone())
                .unwrap_or_else(|| self.environment.scratch.join("tmp"));
            command
                .env("PATH", "/usr/bin:/bin")
                .env("LANG", "C")
                .env("TMPDIR", temporary);
            if self.environment.policy.browser.is_some() {
                // Selects the browser payload syscall profile in the trusted
                // launcher. It is host-controlled and not part of payload env.
                command.env("SAYA_BROWSER_SANDBOX", "1");
            }
        } else {
            command.envs(&self.env);
        }
        let mut descriptors = fds::DescriptorSanitizer::prepare()?;
        let limits = self.environment.policy.resources.clone();
        let environment = self.environment.clone();
        let group = self
            .environment
            .cgroup
            .as_ref()
            .map(cgroup::Cgroup::join_descriptor)
            .transpose()?;
        #[cfg(target_os = "linux")]
        let seccomp = self.seccomp.clone();
        #[cfg(target_os = "linux")]
        let mount_fds = self.mount_fds.clone();
        // SAFETY: the callback only calls descriptor syscalls, write, setrlimit and prctl,
        // using descriptors and values prepared before fork. No allocation.
        unsafe {
            command.pre_exec(move || {
                if environment.closed.load(Ordering::Acquire) {
                    return Err(io::Error::from_raw_os_error(libc::ECANCELED));
                }
                descriptors.mark_close_on_exec()?;
                if let Some(descriptor) = &environment_descriptor {
                    fds::inherit_descriptor(descriptor)?;
                }
                apply_rlimits(&limits)?;
                if let Some(fd) = &group {
                    cgroup::join(fd)?;
                }
                #[cfg(target_os = "linux")]
                if let Some(fd) = &seccomp {
                    linux::inherit_filter_descriptor(fd)?;
                }
                #[cfg(target_os = "linux")]
                for fd in &mount_fds {
                    linux::inherit_filter_descriptor(fd)?;
                }
                Ok(())
            });
        }
        Ok(())
    }
}
impl Drop for Environment {
    fn drop(&mut self) {
        if let Some(group) = &self.cgroup {
            let _ = group.cleanup();
        }
        if self.active_processes.load(Ordering::Acquire) == 0 {
            let _ = fs::remove_dir_all(&self.scratch);
        }
    }
}
pub(crate) struct ProcessLease {
    environment: PreparedEnvironment,
    completed: bool,
}
impl ProcessLease {
    pub(crate) fn complete(&mut self) {
        if !self.completed {
            self.environment
                .inner
                .active_processes
                .fetch_sub(1, Ordering::AcqRel);
            self.completed = true;
        }
    }
}
/// Apply the payload-only Linux filter after trusted socat bridges start.
/// Call in a dedicated child before exec, with undeclared descriptors closed.
/// Other platforms return `sandbox_capability_unavailable`.
pub fn install_linux_payload_filter() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        linux::install_payload_filter()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Linux seccomp is unavailable",
        ))
    }
}
/// A BPF program constructed before fork. Installation performs no allocation.
pub struct PayloadFilter {
    #[cfg(target_os = "linux")]
    inner: linux::Filter,
}
pub fn prepare_linux_payload_filter() -> io::Result<PayloadFilter> {
    #[cfg(target_os = "linux")]
    {
        Ok(PayloadFilter {
            inner: linux::Filter::prepare(),
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(io::Error::from_raw_os_error(libc::ENOTSUP))
    }
}
impl PayloadFilter {
    /// Install only in the payload child after bridges have started.
    pub fn install(&self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            self.inner.install()
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(io::Error::from_raw_os_error(libc::ENOTSUP))
        }
    }
}
fn validate_resources(limits: &ResourceLimits) -> Result<(), BoundaryError> {
    if [
        limits.cpu_seconds,
        limits.file_size_bytes,
        limits.open_files,
        limits.total_memory_bytes,
        limits.total_processes,
    ]
    .into_iter()
    .flatten()
    .any(|value| value == 0)
    {
        return Err(failure(
            "invalid_sandbox_policy",
            "resource limits must be positive",
        ));
    }
    if let Some(quota) = &limits.cpu_quota
        && (quota.quota_micros < 1000 || !(1000..=1_000_000).contains(&quota.period_micros))
    {
        return Err(failure(
            "invalid_sandbox_policy",
            "CPU quota must be at least 1000us and period 1000..1000000us",
        ));
    }
    Ok(())
}
fn apply_rlimits(limits: &ResourceLimits) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    type Resource = libc::c_int;
    #[cfg(target_os = "linux")]
    type Resource = libc::__rlimit_resource_t;
    fn apply(resource: Resource, value: Option<u64>) -> io::Result<()> {
        if let Some(value) = value {
            let value = libc::rlim_t::try_from(value).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "resource limit is outside platform range",
                )
            })?;
            let limit = libc::rlimit {
                rlim_cur: value,
                rlim_max: value,
            };
            // SAFETY: limit is initialized and points to a valid rlimit.
            if unsafe { libc::setrlimit(resource, &limit) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
    apply(libc::RLIMIT_CPU, limits.cpu_seconds)?;
    apply(libc::RLIMIT_FSIZE, limits.file_size_bytes)?;
    apply(libc::RLIMIT_NOFILE, limits.open_files)
}
fn existing_directory(path: &Path) -> Result<PathBuf, BoundaryError> {
    let path = fs::canonicalize(path).map_err(io_error)?;
    if !path.is_dir() {
        return Err(failure(
            "invalid_sandbox_policy",
            "sandbox root must be an existing directory",
        ));
    }
    Ok(path)
}
fn existing_file(path: &Path) -> Result<PathBuf, BoundaryError> {
    let path = fs::canonicalize(path).map_err(io_error)?;
    if !path.is_file() {
        return Err(failure(
            "invalid_sandbox_policy",
            "sandbox read file must be an existing regular file",
        ));
    }
    Ok(path)
}
fn normalize_roots(paths: &[PathBuf]) -> Result<Vec<PathBuf>, BoundaryError> {
    let mut result = paths
        .iter()
        .map(|path| existing_directory(path))
        .collect::<Result<Vec<_>, _>>()?;
    result.sort();
    result.dedup();
    Ok(result)
}
pub(crate) fn normalize_path(path: &Path) -> Result<PathBuf, BoundaryError> {
    if !path.is_absolute()
        || path.as_os_str().as_bytes().contains(&0)
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(failure(
            "invalid_sandbox_policy",
            "sandbox paths must be absolute and contain no parent traversal or NUL",
        ));
    }
    let mut existing = path;
    let mut tail = Vec::new();
    loop {
        match fs::canonicalize(existing) {
            Ok(mut normalized) => {
                for component in tail.iter().rev() {
                    normalized.push(component);
                }
                return Ok(normalized);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                tail.push(
                    existing
                        .file_name()
                        .ok_or_else(|| {
                            failure("invalid_sandbox_policy", "path has no existing ancestor")
                        })?
                        .to_os_string(),
                );
                existing = existing.parent().ok_or_else(|| {
                    failure("invalid_sandbox_policy", "path has no existing ancestor")
                })?;
            }
            Err(error) => return Err(io_error(error)),
        }
    }
}
fn failure(code: &str, message: impl Into<String>) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}
fn io_error(error: io::Error) -> BoundaryError {
    failure("sandbox_prepare_failed", error.to_string())
}
fn unavailable(message: impl Into<String>) -> BoundaryError {
    failure("sandbox_capability_unavailable", message)
}
fn browser_unavailable(message: impl Into<String>) -> BoundaryError {
    failure("browser_sandbox_unavailable", message)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generic_policy_reports_no_browser_evidence_and_keeps_host_behavior() {
        let workspace = tempfile::tempdir().expect("workspace");
        let mut policy = SandboxPolicy::workspace(workspace.path().into());
        policy.backend = BackendKind::Host;
        let environment = prepare(&policy).expect("host environment");
        assert!(!environment.is_browser());
        assert!(environment.browser_directories().is_none());
        assert!(environment.browser_evidence().is_none());
        assert!(environment.identity().browser.is_none());
        assert_eq!(environment.identity().backend, BackendKind::Host);
        environment.cleanup().expect("cleanup");
    }
    #[test]
    fn browser_policy_rejects_host_execution() {
        let install = tempfile::tempdir().expect("installation root");
        let mut policy = SandboxPolicy::browser(BrowserSandboxProfile {
            install_roots: vec![install.path().into()],
        });
        assert!(policy.read_roots.is_empty());
        assert!(policy.write_roots.is_empty());
        assert!(policy.protected_paths.is_empty());
        assert!(policy.browser.is_some());
        policy.backend = BackendKind::Host;
        let error = prepare(&policy).expect_err("host cannot isolate a browser");
        assert_eq!(error.0.code, "browser_sandbox_unavailable");
        assert!(error.0.message.contains("host"));
    }
    #[test]
    fn browser_policy_rejects_generic_roots_and_missing_installation_paths() {
        let workspace = tempfile::tempdir().expect("workspace");
        let mut policy = SandboxPolicy::workspace(workspace.path().into());
        policy.backend = BackendKind::Host;
        policy.browser = Some(BrowserSandboxProfile {
            install_roots: vec![workspace.path().into()],
        });
        assert_eq!(
            prepare(&policy).expect_err("generic roots").0.code,
            "invalid_sandbox_policy"
        );
        let mut policy = SandboxPolicy::browser(BrowserSandboxProfile {
            install_roots: vec![workspace.path().join("missing")],
        });
        policy.backend = BackendKind::Host;
        let error = prepare(&policy).expect_err("missing installation root");
        assert_eq!(error.0.code, "invalid_sandbox_policy");
        assert!(error.0.message.contains("unavailable"));
        let empty = SandboxPolicy::browser(BrowserSandboxProfile {
            install_roots: Vec::new(),
        });
        assert_eq!(
            prepare(&empty).expect_err("empty roots").0.code,
            "invalid_sandbox_policy"
        );
        let configuration = tempfile::tempdir().expect("configuration fixture");
        let saya = configuration.path().join(".saya");
        fs::create_dir(&saya).expect("configuration root");
        let policy = SandboxPolicy::browser(BrowserSandboxProfile {
            install_roots: vec![saya],
        });
        let error = prepare(&policy).expect_err("Saya configuration root");
        assert_eq!(error.0.code, "invalid_sandbox_policy");
        assert!(error.0.message.contains("configuration"));
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
            && home.is_dir()
        {
            let policy = SandboxPolicy::browser(BrowserSandboxProfile {
                install_roots: vec![home],
            });
            assert_eq!(
                prepare(&policy).expect_err("whole home directory").0.code,
                "invalid_sandbox_policy"
            );
        }
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_browser_policy_fails_without_weakening_chromium_sandbox() {
        let install = tempfile::tempdir().expect("installation root");
        let policy = SandboxPolicy::browser(BrowserSandboxProfile {
            install_roots: vec![install.path().into()],
        });
        let error = prepare(&policy).expect_err("macOS Seatbelt cannot host Chromium");
        assert_eq!(error.0.code, "browser_sandbox_unavailable");
        assert!(error.0.message.contains("sandbox_apply"));
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_browser_policy_never_falls_back_to_another_backend() {
        let install = tempfile::tempdir().expect("installation root");
        let mut policy = SandboxPolicy::browser(BrowserSandboxProfile {
            install_roots: vec![install.path().into()],
        });
        policy.backend = BackendKind::Seatbelt;
        assert_eq!(
            prepare(&policy)
                .expect_err("browser never uses Seatbelt on Linux")
                .0
                .code,
            "browser_sandbox_unavailable"
        );
    }
}
