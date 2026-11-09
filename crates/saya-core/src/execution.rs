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
}
#[derive(Debug, Clone)]
pub struct SandboxPolicy {
    pub backend: BackendKind,
    pub workspace: PathBuf,
    pub read_roots: Vec<PathBuf>,
    pub write_roots: Vec<PathBuf>,
    /// Literal denied paths. Unsupported masks fail preparation.
    pub protected_paths: Vec<PathBuf>,
    pub network: NetworkPolicy,
    pub resources: ResourceLimits,
    /// An explicitly delegated cgroup v2 subtree. No global tree is modified.
    pub cgroup_root: Option<PathBuf>,
}
impl SandboxPolicy {
    pub fn workspace(workspace: PathBuf) -> Self {
        Self {
            backend: BackendKind::Auto,
            read_roots: vec![workspace.clone()],
            write_roots: vec![workspace.clone()],
            workspace,
            protected_paths: Vec::new(),
            network: NetworkPolicy::Deny,
            resources: ResourceLimits::default(),
            cgroup_root: None,
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
    policy.protected_paths = policy
        .protected_paths
        .iter()
        .map(|path| normalize_path(path))
        .collect::<Result<Vec<_>, _>>()?;
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
    let base = [std::env::temp_dir(), PathBuf::from("/var/tmp")].into_iter().filter_map(|path| fs::canonicalize(path).ok()).find(|path| {
        !path.starts_with(&policy.workspace) && !policy.write_roots.iter().any(|root| path.starts_with(root))
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
        Some(linux::LinuxBackend::prepare(&policy, &scratch)?)
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
    let identity = EnvironmentIdentity {
        id: uuid::Uuid::now_v7().to_string(),
        backend: policy.backend,
        host_os: std::env::consts::OS.into(),
        execution_os: std::env::consts::OS.into(),
        architecture: std::env::consts::ARCH.into(),
        policy_digest: digest,
    };
    Ok(PreparedEnvironment {
        inner: Arc::new(Environment {
            policy,
            identity,
            capabilities,
            scratch,
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
        let temporary = if self.inner.policy.backend == BackendKind::Bubblewrap {
            PathBuf::from("/tmp")
        } else {
            self.inner.scratch.join("tmp")
        };
        env.insert("TMPDIR".into(), temporary.into_os_string());
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
        if self.inner.policy.backend != BackendKind::Host
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
            command
                .env("PATH", "/usr/bin:/bin")
                .env("LANG", "C")
                .env("TMPDIR", self.environment.scratch.join("tmp"));
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
