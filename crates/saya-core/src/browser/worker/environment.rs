use super::*;

/// One private, host-owned temp tree for a worker lifecycle.
pub(super) struct TempArtifacts {
    pub(super) base: PathBuf,
    pub(super) config_path: PathBuf,
    pub(super) output_dir: PathBuf,
    pub(super) profile_dir: PathBuf,
    pub(super) work_dir: PathBuf,
    pub(super) temp_dir: PathBuf,
    pub(super) active: bool,
}

fn create_private_directory(path: &Path) -> Result<(), BoundaryError> {
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    builder.create(path).map_err(|_| {
        error(
            "worker_environment",
            "Browser worker directory cannot be created",
        )
    })
}

impl TempArtifacts {
    pub(super) fn create(settings: &BrowserWorkerSettings) -> Result<Self, BoundaryError> {
        let root = settings
            .temp_root
            .clone()
            .unwrap_or_else(std::env::temp_dir);
        let root = fs::canonicalize(&root).map_err(|_| {
            error(
                "worker_environment",
                "Browser worker temp root is unavailable",
            )
        })?;
        if !root.is_dir() {
            return Err(error(
                "worker_environment",
                "Browser worker temp root is not a directory",
            ));
        }
        for _ in 0..16 {
            let base = root.join(format!(
                "saya-browser-worker-{}",
                uuid::Uuid::now_v7().simple()
            ));
            match create_private_directory(&base) {
                Ok(()) => {
                    let config_dir = base.join("config");
                    let output_dir = base.join("output");
                    let profile_dir = base.join("profile");
                    let work_dir = base.join("work");
                    let temp_dir = work_dir.join("tmp");
                    let result = (|| {
                        for path in [&config_dir, &output_dir, &profile_dir, &work_dir, &temp_dir] {
                            create_private_directory(path)?;
                        }
                        create_private_directory(&profile_dir.join("cache"))?;
                        Ok(())
                    })();
                    if let Err(error) = result {
                        let _ = fs::remove_dir_all(&base);
                        return Err(error);
                    }
                    return Ok(Self {
                        config_path: config_dir.join("config.json"),
                        base,
                        output_dir,
                        profile_dir,
                        work_dir,
                        temp_dir,
                        active: true,
                    });
                }
                Err(_) if base.exists() => continue,
                Err(error) => return Err(error),
            }
        }
        Err(error(
            "worker_environment",
            "Browser worker temp directory could not be allocated",
        ))
    }

    pub(super) fn remove(&mut self) -> Result<(), BoundaryError> {
        if !self.active {
            return Ok(());
        }
        match fs::remove_dir_all(&self.base) {
            Ok(()) => {
                self.active = false;
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.active = false;
                Ok(())
            }
            Err(_) => Err(error(
                "worker_cleanup",
                "Browser worker temp files could not be removed",
            )),
        }
    }

    pub(super) fn directories(&self) -> BrowserSandboxDirectories {
        BrowserSandboxDirectories {
            profile: self.profile_dir.clone(),
            output: self.output_dir.clone(),
            tmp: self.temp_dir.clone(),
            work: self.work_dir.clone(),
            config: self
                .config_path
                .parent()
                .unwrap_or(&self.base)
                .to_path_buf(),
        }
    }
}

impl Drop for TempArtifacts {
    fn drop(&mut self) {
        if self.active {
            let _ = fs::remove_dir_all(&self.base);
        }
    }
}

/// Validate a host-supplied upload name without rewriting it. The page must
/// receive exactly the artifact's stored name; only unsafe path components are
/// rejected.
fn validate_worker_filename(value: &str) -> Result<&str, BrowserFailure> {
    let name = value
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .ok_or_else(|| {
            BrowserFailure::new("file_name_invalid", "Browser upload name is invalid")
        })?;
    if name != value
        || name.starts_with('.')
        || name.len() > 128
        || name.chars().any(char::is_control)
        || name.contains('\0')
    {
        return Err(BrowserFailure::new(
            "file_name_invalid",
            "Browser upload name is unsafe",
        ));
    }
    Ok(name)
}

/// Remove a staging directory unless it was disarmed after a complete write.
struct UploadDirectoryGuard {
    pub(super) path: Option<PathBuf>,
}

impl Drop for UploadDirectoryGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = fs::remove_dir_all(&path);
        }
    }
}

pub(super) fn stage_worker_upload(
    work_dir: &Path,
    filename: &str,
    bytes: &[u8],
    cancel: &dyn Cancellation,
) -> Result<PathBuf, BrowserFailure> {
    if cancel.is_cancelled() {
        return Err(BrowserFailure::new(
            "cancelled",
            "Browser upload staging cancelled",
        ));
    }
    let name = validate_worker_filename(filename)?;
    let uploads = work_dir.join("uploads");
    match DirBuilder::new().mode(0o700).create(&uploads) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => {
            return Err(BrowserFailure::new(
                "file_broker",
                "Browser upload staging directory cannot be created",
            ));
        }
    }
    let metadata = fs::symlink_metadata(&uploads)
        .map_err(|_| BrowserFailure::new("file_broker", "Browser upload staging is unavailable"))?;
    // SAFETY: geteuid has no pointer arguments or ownership requirements.
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(BrowserFailure::new(
            "file_broker",
            "Browser upload staging must be a private owned directory",
        ));
    }
    // A per-upload directory keeps the page-visible basename identical to the
    // artifact name while remaining collision-free.
    let directory = uploads.join(format!("upload-{}", uuid::Uuid::now_v7().simple()));
    DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .map_err(|_| {
            BrowserFailure::new(
                "file_broker",
                "Browser upload staging directory cannot be created",
            )
        })?;
    let mut guard = UploadDirectoryGuard {
        path: Some(directory.clone()),
    };
    let path = directory.join(name);
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = options.open(&path).map_err(|_| {
        BrowserFailure::new(
            "file_broker",
            "Browser upload staging file cannot be created",
        )
    })?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| {
            BrowserFailure::new(
                "file_broker",
                "Browser upload staging file cannot be written or synced",
            )
        })?;
    drop(file);
    if cancel.is_cancelled() {
        return Err(BrowserFailure::new(
            "cancelled",
            "Browser upload staging cancelled",
        ));
    }
    guard.path = None;
    Ok(path)
}

struct UploadCleanupCancel;
impl Cancellation for UploadCleanupCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}

/// Best-effort staging cleanup for a cancelled or dropped upload. Failure is
/// never reported as a successful upload; the Run-private directory is also
/// covered by the worker/Run cleanup path.
pub(super) struct RemoteUploadGuard {
    pub(super) connection: Option<Arc<dyn WorkerConnection>>,
    pub(super) path: String,
}

impl Drop for RemoteUploadGuard {
    fn drop(&mut self) {
        let Some(connection) = self.connection.take() else {
            return;
        };
        let path = std::mem::take(&mut self.path);
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        runtime.spawn(async move {
            let _ = connection.remove_upload(&path, &UploadCleanupCancel).await;
        });
    }
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), BoundaryError> {
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = options.open(path).map_err(|_| {
        error(
            "worker_environment",
            "Browser worker config cannot be created",
        )
    })?;
    file.write_all(bytes).map_err(|_| {
        error(
            "worker_environment",
            "Browser worker config cannot be written",
        )
    })?;
    Ok(())
}

/// A fixed host-generated launch request. The injectable launcher makes offline
/// tests deterministic; production assembly uses [`ManagedProcessLauncher`].
#[derive(Debug, Clone)]
pub struct WorkerLaunchRequest {
    pub argv: Vec<OsString>,
    pub cwd: PathBuf,
    pub env: BTreeMap<OsString, OsString>,
    pub timeout: Duration,
    pub stderr_limit: usize,
    pub mcp_limits: McpLimits,
    /// Run-private directory layout that the launch inputs were derived from.
    pub directories: BrowserSandboxDirectories,
    /// The fresh isolation environment for this worker lifecycle. Production
    /// launches require it; offline fixtures may omit it.
    pub execution: Option<PreparedEnvironment>,
    /// Identity verified immediately before this launch. Remote backends use
    /// these resolved paths instead of re-deriving them from settings.
    pub identity: Option<VerifiedWorkerIdentity>,
}

fn os_path(path: &Path) -> Result<&str, BoundaryError> {
    path.to_str()
        .ok_or_else(|| invalid_config("Browser worker paths must be UTF-8"))
}

fn build_launch_request(
    settings: &BrowserWorkerSettings,
    identity: &VerifiedWorkerIdentity,
    directories: &BrowserSandboxDirectories,
) -> Result<WorkerLaunchRequest, BoundaryError> {
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(settings.connect_timeout_ms))
        .ok_or_else(|| identity_error("Browser worker image recheck deadline is out of range"))?;
    let mut checkpoint = || {
        if Instant::now() >= deadline {
            return Err(identity_error(
                "Browser worker image recheck exceeded its deadline",
            ));
        }
        Ok(())
    };
    identity.check_local_executables(&mut checkpoint)?;
    // Chromium's own sandbox stays enabled. The browser worker network policy
    // is always decided by the host environment; `worker.proxy` is validated
    // input reserved for the later broker stage and must not be injected here.
    let launch_options = json!({
        "headless": true,
        "chromiumSandbox": true,
        "executablePath": os_path(&identity.chromium)?,
    });
    let config = json!({
        "browser": {
            "browserName": "chromium",
            "isolated": true,
            "launchOptions": launch_options,
        },
        "outputDir": os_path(&directories.output)?,
        "imageResponses": "omit",
        "webmcp": false,
    });
    let bytes = serde_json::to_vec_pretty(&config).map_err(|_| {
        error(
            "worker_environment",
            "Browser worker config cannot be encoded",
        )
    })?;
    let config_path = directories.config.join("config.json");
    write_private_file(&config_path, &bytes)?;

    let argv = vec![
        OsString::from(os_path(&identity.node)?),
        OsString::from(os_path(&identity.cli_js)?),
        OsString::from("--config"),
        config_path.into_os_string(),
        OsString::from("--headless"),
        OsString::from("--isolated"),
        OsString::from("--sandbox"),
        OsString::from("--block-service-workers"),
        OsString::from("--no-webmcp"),
        OsString::from("--ignore-roots"),
        OsString::from("--image-responses"),
        OsString::from("omit"),
        OsString::from("--output-dir"),
        directories.output.clone().into_os_string(),
        OsString::from("--executable-path"),
        identity.chromium.clone().into_os_string(),
    ];

    let mut env = BTreeMap::new();
    env.insert(OsString::from("PATH"), OsString::from("/usr/bin:/bin"));
    env.insert(
        OsString::from("HOME"),
        directories.profile.clone().into_os_string(),
    );
    env.insert(
        OsString::from("TMPDIR"),
        directories.tmp.clone().into_os_string(),
    );
    env.insert(
        OsString::from("XDG_CACHE_HOME"),
        directories.profile.join("cache").into_os_string(),
    );
    env.insert(
        OsString::from("PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD"),
        OsString::from("1"),
    );
    env.insert(OsString::from("LANG"), OsString::from("C"));
    env.insert(OsString::from("NO_COLOR"), OsString::from("1"));

    Ok(WorkerLaunchRequest {
        argv,
        cwd: directories.work.clone(),
        env,
        timeout: Duration::from_millis(settings.request_timeout_ms.saturating_add(30_000)),
        stderr_limit: settings.stderr_limit_bytes,
        mcp_limits: McpLimits {
            connect_timeout: Duration::from_millis(settings.connect_timeout_ms),
            request_timeout: Duration::from_millis(settings.request_timeout_ms),
            max_wire_bytes: MAX_WORKER_SCREENSHOT_WIRE_BYTES,
            max_pages: 32,
            max_tools: 256,
            max_catalog_bytes: MAX_WORKER_RESULT_BYTES,
            max_result_bytes: MAX_WORKER_RESULT_BYTES,
            max_content_blocks: 256,
        },
        directories: directories.clone(),
        execution: None,
        identity: Some(identity.clone()),
    })
}

pub(crate) fn environment_digest(env: &BTreeMap<OsString, OsString>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"saya-browser-worker-env-v1\0");
    for (key, value) in env {
        hasher.update(key.as_os_str().as_bytes());
        hasher.update([0]);
        hasher.update(value.as_os_str().as_bytes());
        hasher.update([0]);
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(super) fn map_environment_failure(failure: &BoundaryError) -> BrowserFailure {
    match failure.0.code.as_str() {
        "browser_sandbox_unavailable" => BrowserFailure::new(
            "browser_sandbox_unavailable",
            "Host execution cannot provide a Chromium sandbox for the browser worker",
        ),
        "browser_vm_unavailable" => BrowserFailure::new(
            "browser_vm_unavailable",
            "The dedicated browser VM host channel is unavailable",
        ),
        _ => BrowserFailure::new(
            "worker_environment",
            "Browser sandbox environment could not be prepared",
        ),
    }
}

pub(super) fn map_mcp_failure(failure: BoundaryError) -> BrowserFailure {
    match failure.0.code.as_str() {
        "outcome_unknown" => BrowserFailure::uncertain(
            "Browser worker call completion could not be confirmed. The call was not replayed",
        ),
        "cancelled" => {
            BrowserFailure::new("cancelled", "Browser worker call cancelled before dispatch")
        }
        "mcp_stale_catalog" => BrowserFailure::new(
            "stale_catalog",
            "Browser worker catalogue changed; begin a new session",
        ),
        "mcp_unknown_tool" => {
            BrowserFailure::new("worker_catalog", "Browser worker tool is unavailable")
        }
        "invalid_args" => BrowserFailure::new(
            "invalid_worker_args",
            "Browser worker rejected the host request before dispatch",
        ),
        "mcp_limit" => BrowserFailure::new(
            "worker_limit",
            "Browser worker request exceeded a host limit",
        ),
        "needs_input" => BrowserFailure::new(
            "worker_input",
            "Browser worker requested additional input, which this provider does not supply",
        ),
        "mcp_timeout" | "mcp_schema" => BrowserFailure::uncertain(
            "Browser worker call completion could not be confirmed. The call was not replayed",
        ),
        _ => BrowserFailure::new(
            "worker_transport",
            "Browser worker call failed before a terminal result",
        ),
    }
}

/// Creates a fresh browser isolation environment for one worker lifecycle.
/// Implementations prepare a new environment for every launch; a closed
/// environment is never reused.
pub trait BrowserEnvironmentFactory: Send + Sync {
    fn prepare(&self) -> Result<Arc<dyn BrowserWorkspaceLease>, BoundaryError>;
}

/// One run-private workspace lease. `release` must reap nothing itself; the
/// provider reaps the worker and Chromium process group first, then calls this
/// to await lease cleanup and remove the private directories.
#[async_trait]
pub trait BrowserWorkspaceLease: Send + Sync {
    fn directories(&self) -> &BrowserSandboxDirectories;
    /// The prepared native environment for production launches. Offline test
    /// leases return `None`.
    fn execution(&self) -> Option<PreparedEnvironment>;
    async fn release(&self) -> Result<(), BoundaryError>;
}

struct NativeBrowserWorkspace {
    pub(super) directories: BrowserSandboxDirectories,
    pub(super) environment: PreparedEnvironment,
}

#[async_trait]
impl BrowserWorkspaceLease for NativeBrowserWorkspace {
    fn directories(&self) -> &BrowserSandboxDirectories {
        &self.directories
    }

    fn execution(&self) -> Option<PreparedEnvironment> {
        Some(self.environment.clone())
    }

    async fn release(&self) -> Result<(), BoundaryError> {
        self.environment.wait_for_process_cleanup().await?;
        self.environment.cleanup().map(|_| ())
    }
}

/// Production factory over the native browser sandbox. Construction performs a
/// preflight preparation and cleanup so unsupported hosts fail before a worker
/// or Chromium process can start.
pub struct NativeBrowserEnvironmentFactory {
    pub(super) install_roots: Vec<PathBuf>,
    pub(super) backend: BackendKind,
}

impl NativeBrowserEnvironmentFactory {
    pub fn new(install_roots: Vec<PathBuf>, backend: BackendKind) -> Result<Self, BoundaryError> {
        let factory = Self {
            install_roots,
            backend,
        };
        factory.preflight()?;
        Ok(factory)
    }

    pub(super) fn policy(&self) -> SandboxPolicy {
        let mut policy = SandboxPolicy::browser(BrowserSandboxProfile {
            install_roots: self.install_roots.clone(),
        });
        policy.backend = self.backend;
        policy
    }

    pub(super) fn preflight(&self) -> Result<(), BoundaryError> {
        let environment = crate::execution::prepare(&self.policy())?;
        environment.cleanup().map(|_| ())
    }
}

impl BrowserEnvironmentFactory for NativeBrowserEnvironmentFactory {
    fn prepare(&self) -> Result<Arc<dyn BrowserWorkspaceLease>, BoundaryError> {
        let environment = crate::execution::prepare(&self.policy())?;
        let Some(directories) = environment.browser_directories().cloned() else {
            let _ = environment.cleanup();
            return Err(error(
                "browser_sandbox_unavailable",
                "Browser sandbox did not establish run-private browser directories",
            ));
        };
        if !environment.is_browser() || environment.identity().browser.is_none() {
            let _ = environment.cleanup();
            return Err(error(
                "browser_sandbox_unavailable",
                "Browser sandbox did not establish a browser environment",
            ));
        }
        Ok(Arc::new(NativeBrowserWorkspace {
            directories,
            environment,
        }))
    }
}

/// Injectable worker launch/transport boundary.
#[async_trait]
pub trait WorkerLauncher: Send + Sync {
    async fn launch(
        &self,
        request: WorkerLaunchRequest,
        cancel: &dyn Cancellation,
    ) -> Result<Arc<dyn WorkerConnection>, BrowserFailure>;

    /// Retry cleanup that a previous launch could not confirm. Backends that
    /// keep no remote state succeed immediately. A failure keeps the handle
    /// registered, so a later shutdown or launch retries instead of reporting
    /// a second false success.
    async fn retry_pending_cleanup(&self) -> Result<(), BrowserFailure> {
        Ok(())
    }
}

/// Verifies the exact worker identity the selected backend will launch. The
/// local implementation reads host files; a backend with a remote worker
/// verifies the same manifest and named files through its controlled channel.
#[async_trait]
pub trait WorkerIdentityVerifier: Send + Sync {
    /// Construction-time check. Local backends verify files here; remote
    /// backends validate settings and local connection inputs without starting
    /// a worker.
    fn preflight(&self) -> Result<(), BoundaryError>;

    /// Verification immediately before a launch. Must not be satisfied by a
    /// transport-level success status alone.
    async fn verify(
        &self,
        cancel: &dyn Cancellation,
    ) -> Result<VerifiedWorkerIdentity, BrowserFailure>;
}

/// A backend-provided verifier over the host-named worker files.
pub struct LocalWorkerIdentityVerifier {
    pub(super) settings: BrowserWorkerSettings,
}

impl LocalWorkerIdentityVerifier {
    pub fn new(settings: BrowserWorkerSettings) -> Self {
        Self { settings }
    }
}

#[async_trait]
impl WorkerIdentityVerifier for LocalWorkerIdentityVerifier {
    fn preflight(&self) -> Result<(), BoundaryError> {
        verify_worker_identity(&self.settings).map(|_| ())
    }

    async fn verify(
        &self,
        cancel: &dyn Cancellation,
    ) -> Result<VerifiedWorkerIdentity, BrowserFailure> {
        if cancel.is_cancelled() {
            return Err(BrowserFailure::new(
                "cancelled",
                "Browser worker identity check cancelled before dispatch",
            ));
        }
        verify_worker_identity_checked(&self.settings, Some(cancel)).map_err(|failure| {
            if failure.0.code == "cancelled" {
                BrowserFailure::new(
                    "cancelled",
                    "Browser worker identity verification cancelled before launch",
                )
            } else {
                BrowserFailure::new(
                    "worker_identity",
                    "Browser worker identity changed before launch",
                )
            }
        })
    }
}

/// Builds the exact launch request for a backend. Provider lifecycle,
/// catalogue validation, and tool mapping stay shared across backends.
pub trait WorkerLaunchPlanner: Send + Sync {
    fn build(
        &self,
        identity: &VerifiedWorkerIdentity,
        directories: &BrowserSandboxDirectories,
    ) -> Result<WorkerLaunchRequest, BoundaryError>;
}

/// Local planner over host-resolved Node, worker and Chromium paths.
pub struct LocalWorkerLaunchPlanner {
    pub(super) settings: BrowserWorkerSettings,
}

impl LocalWorkerLaunchPlanner {
    pub fn new(settings: BrowserWorkerSettings) -> Self {
        Self { settings }
    }
}

impl WorkerLaunchPlanner for LocalWorkerLaunchPlanner {
    fn build(
        &self,
        identity: &VerifiedWorkerIdentity,
        directories: &BrowserSandboxDirectories,
    ) -> Result<WorkerLaunchRequest, BoundaryError> {
        build_launch_request(&self.settings, identity, directories)
    }
}

/// Host-only roles reported by the fixed private worker protocol.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkerProcessInfo {
    pub schema_version: u32,
    pub node_pid: u32,
    pub node_pid_namespace: WorkerPidNamespace,
    pub browser_pid: u32,
    pub renderer_pids: Vec<u32>,
}

impl WorkerProcessInfo {
    pub(crate) fn decode(value: Value) -> Result<Self, BrowserFailure> {
        let info: Self = serde_json::from_value(value).map_err(|_| process_info_unavailable())?;
        let namespace_valid = [&info.node_pid_namespace.dev, &info.node_pid_namespace.ino]
            .into_iter()
            .all(|value| {
                value
                    .parse::<u64>()
                    .is_ok_and(|number| number != 0 && number.to_string() == *value)
            });
        let mut pids = BTreeSet::from([info.node_pid, info.browser_pid]);
        if info.schema_version != 1
            || info.node_pid == 0
            || info.browser_pid == 0
            || pids.len() != 2
            || !namespace_valid
            || info.renderer_pids.len() > MAX_SANDBOX_PROCESSES
            || info
                .renderer_pids
                .iter()
                .any(|pid| *pid == 0 || !pids.insert(*pid))
        {
            return Err(process_info_unavailable());
        }
        Ok(info)
    }
}

fn process_info_unavailable() -> BrowserFailure {
    BrowserFailure::new(
        "worker_process_info",
        "Browser worker process information is unavailable",
    )
}

pub(crate) fn process_info_failure(failure: BoundaryError) -> BrowserFailure {
    if failure.0.code == "cancelled" {
        BrowserFailure::new("cancelled", "Browser process information query cancelled")
    } else {
        process_info_unavailable()
    }
}

pub(crate) fn capture_screenshot_failure(failure: BoundaryError) -> BrowserFailure {
    match failure.0.code.as_str() {
        "cancelled" => BrowserFailure::new("cancelled", "Browser screenshot cancelled"),
        "browser_capability_disabled" => {
            BrowserFailure::new("browser_capability_disabled", failure.0.message)
        }
        "saya_capture_timeout" | "mcp_timeout" => {
            BrowserFailure::new("saya_capture_timeout", failure.0.message)
        }
        "saya_capture_stale_context" => {
            BrowserFailure::new("saya_capture_stale_context", failure.0.message)
        }
        "saya_capture_incomplete" => {
            BrowserFailure::new("saya_capture_incomplete", failure.0.message)
        }
        "saya_capture_busy" => BrowserFailure::new("saya_capture_busy", failure.0.message),
        "worker_closed" | "mcp_disconnect" => {
            BrowserFailure::new("worker_closed", failure.0.message)
        }
        "browser_limit" | "mcp_limit" => {
            BrowserFailure::new("browser_limit", "Browser screenshot exceeds its byte limit")
        }
        _ => BrowserFailure::new("worker_screenshot_failed", failure.0.message),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapturedScreenshotReply {
    pub(super) schema_version: u32,
    pub(super) protocol_version: u32,
    pub(super) mime: String,
    pub(super) data: String,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) masked: bool,
    pub(super) url: String,
}

pub(super) fn parse_captured_screenshot(
    value: Value,
) -> Result<crate::browser::BrowserScreenshot, BrowserFailure> {
    let reply: CapturedScreenshotReply = serde_json::from_value(value).map_err(|_| {
        BrowserFailure::new(
            "invalid_provider_result",
            "Browser worker returned an invalid screenshot payload",
        )
    })?;
    if reply.schema_version != 1 || reply.protocol_version != 1 {
        return Err(BrowserFailure::new(
            "invalid_provider_result",
            "Browser worker returned an unsupported screenshot protocol version",
        ));
    }
    if !reply.masked {
        return Err(BrowserFailure::new(
            "browser_capability_disabled",
            "Browser provider did not confirm sensitive-field masking",
        ));
    }
    if reply.mime != "image/png" {
        return Err(BrowserFailure::new(
            "invalid_provider_result",
            "Browser worker returned an unsupported screenshot MIME type",
        ));
    }
    if reply.width == 0
        || reply.width > MAX_SCREENSHOT_DIMENSION
        || reply.height == 0
        || reply.height > MAX_SCREENSHOT_DIMENSION
    {
        return Err(BrowserFailure::new(
            "invalid_provider_result",
            "Browser worker returned invalid screenshot dimensions",
        ));
    }
    crate::browser::provider_url(&reply.url)?;
    if reply.data.is_empty() {
        return Err(BrowserFailure::new(
            "invalid_provider_result",
            "Browser worker returned empty screenshot data",
        ));
    }
    if reply.data.len() > MAX_SCREENSHOT_BASE64_BYTES {
        return Err(BrowserFailure::new(
            "browser_limit",
            "Browser screenshot exceeds its byte limit",
        ));
    }
    let bytes = STANDARD.decode(&reply.data).map_err(|_| {
        BrowserFailure::new(
            "invalid_provider_result",
            "Browser worker returned invalid base64 screenshot data",
        )
    })?;
    if bytes.len() > crate::media::MAX_IMAGE_BYTES as usize {
        return Err(BrowserFailure::new(
            "browser_limit",
            "Browser screenshot exceeds its byte limit",
        ));
    }
    if bytes.len() < 33
        || &bytes[..8] != b"\x89PNG\r\n\x1a\n"
        || u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) != 13
        || &bytes[12..16] != b"IHDR"
    {
        return Err(BrowserFailure::new(
            "invalid_provider_result",
            "Browser worker returned a malformed PNG screenshot",
        ));
    }
    let png_width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let png_height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    if png_width == 0 || png_height == 0 || png_width != reply.width || png_height != reply.height {
        return Err(BrowserFailure::new(
            "invalid_provider_result",
            "Browser worker returned mismatched PNG screenshot dimensions",
        ));
    }
    Ok(crate::browser::BrowserScreenshot {
        mime: "image/png".into(),
        filename: "screenshot.png".into(),
        bytes,
        width: png_width,
        height: png_height,
        masked: true,
        url: Some(reply.url),
    })
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkerPidNamespace {
    pub dev: String,
    pub ino: String,
}

/// A live worker connection. Implementations must send each call at most once.
#[async_trait]
pub trait WorkerConnection: Send + Sync {
    fn protocol_version(&self) -> Option<String>;
    fn generation(&self) -> u64;
    /// The supervised worker process-group leader, when this is a real
    /// process. Offline fixtures return `None` and skip procfs inspection.
    fn process_id(&self) -> Option<u32> {
        None
    }
    /// Bounded stderr tail used as supplemental sandbox evidence.
    fn stderr_tail(&self) -> Vec<u8> {
        Vec::new()
    }
    /// Obtain an opaque handle proving a live native launch process and its
    /// pinned local baseline exist for kernel sandbox verification.
    async fn native_sandbox_context(
        &self,
        _cancel: &dyn Cancellation,
    ) -> Result<NativeSandboxContext<'_>, BrowserFailure> {
        Err(BrowserFailure::new(
            "browser_sandbox_unavailable",
            "Browser worker native sandbox context is unavailable",
        ))
    }
    /// Read fixed private roles without creating a browser or exposing a tool.
    async fn process_info(
        &self,
        _cancel: &dyn Cancellation,
    ) -> Result<WorkerProcessInfo, BrowserFailure> {
        Err(BrowserFailure::new(
            "worker_process_info",
            "Browser worker process information is unavailable",
        ))
    }
    /// Capture a host-only masked PNG screenshot through the private worker channel.
    async fn capture_screenshot(
        &self,
        _cancel: &dyn Cancellation,
    ) -> Result<Value, BrowserFailure> {
        Err(BrowserFailure::new(
            "browser_capability_disabled",
            "The browser worker does not support masked screenshot capture",
        ))
    }
    async fn list_tools(&self, cancel: &dyn Cancellation) -> Result<McpCatalog, BrowserFailure>;
    async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        generation: u64,
        cancel: &dyn Cancellation,
    ) -> Result<McpToolResult, BrowserFailure>;
    /// Copy one host-staged upload into the worker's Run-private staging area
    /// and return the absolute path the worker may pass to
    /// `browser_file_upload`.
    async fn put_upload(
        &self,
        _filename: &str,
        _bytes: &[u8],
        _cancel: &dyn Cancellation,
    ) -> Result<String, BrowserFailure> {
        Err(BrowserFailure::unsupported("upload staging"))
    }
    /// Remove one staging upload after the fixed upload tool call. The path is
    /// the value previously returned by `put_upload`.
    async fn remove_upload(
        &self,
        _path: &str,
        _cancel: &dyn Cancellation,
    ) -> Result<(), BrowserFailure> {
        Ok(())
    }
    /// Read one download file produced by this worker. Implementations must
    /// confine the path to the Run's output directory and stay bounded.
    async fn read_download(
        &self,
        _path: &str,
        _max_bytes: usize,
        _cancel: &dyn Cancellation,
    ) -> Result<Vec<u8>, BrowserFailure> {
        Err(BrowserFailure::unsupported("download read"))
    }
    /// List regular files currently present in the Run-private download
    /// directory. Only the provider's own event for this click may name a
    /// download; this listing proves the named file is newly produced.
    async fn list_downloads(
        &self,
        _cancel: &dyn Cancellation,
    ) -> Result<BTreeSet<String>, BrowserFailure> {
        Err(BrowserFailure::unsupported("download listing"))
    }
    async fn shutdown(&self) -> Result<(), BrowserFailure>;
}
