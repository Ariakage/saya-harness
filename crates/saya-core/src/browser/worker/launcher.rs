use super::*;

pub(super) struct RealWorkerConnection {
    pub(super) client: AsyncMutex<Option<crate::mcp::McpClient>>,
    pub(super) process: Arc<AsyncMutex<Option<ManagedProcess>>>,
    pub(super) protocol: String,
    pub(super) generation: std::sync::atomic::AtomicU64,
    pub(super) pid: u32,
    pub(super) stderr_tail: Arc<std::sync::Mutex<VecDeque<u8>>>,
    pub(super) work_dir: PathBuf,
    pub(super) output_dir: PathBuf,
    pub(super) local_images: Option<Arc<VerifiedLocalImages>>,
    pub(super) node_path: PathBuf,
    pub(super) chromium_path: PathBuf,
    pub(super) baseline: Option<NativeLaunchBaseline>,
    #[cfg(test)]
    pub(super) test_sandbox_ready: bool,
}

#[async_trait]
impl WorkerConnection for RealWorkerConnection {
    fn protocol_version(&self) -> Option<String> {
        Some(self.protocol.clone())
    }

    fn generation(&self) -> u64 {
        self.generation.load(std::sync::atomic::Ordering::Acquire)
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

    async fn native_sandbox_context(
        &self,
        cancel: &dyn Cancellation,
    ) -> Result<NativeSandboxContext<'_>, BrowserFailure> {
        if cancel.is_cancelled() {
            return Err(BrowserFailure::new(
                "cancelled",
                "Chromium sandbox verification cancelled",
            ));
        }
        let process_guard = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return Err(BrowserFailure::new(
                    "cancelled",
                    "Chromium sandbox verification cancelled",
                ));
            }
            guard = self.process.clone().lock_owned() => guard,
        };
        #[cfg(test)]
        let process_live = if self.test_sandbox_ready {
            true
        } else {
            let Some(process) = process_guard.as_ref() else {
                return Err(BrowserFailure::new(
                    "browser_sandbox_unavailable",
                    "Browser worker process handle is unavailable",
                ));
            };
            !process.has_exited().unwrap_or(true)
        };
        #[cfg(not(test))]
        let process_live = {
            let Some(process) = process_guard.as_ref() else {
                return Err(BrowserFailure::new(
                    "browser_sandbox_unavailable",
                    "Browser worker process handle is unavailable",
                ));
            };
            !process.has_exited().unwrap_or(true)
        };
        if !process_live {
            return Err(BrowserFailure::new(
                "browser_sandbox_unavailable",
                "Browser worker supervisor has already exited",
            ));
        }
        let local_images = self.local_images.clone().ok_or_else(|| {
            BrowserFailure::new(
                "browser_sandbox_unavailable",
                "Browser worker local executable identities are unavailable",
            )
        })?;
        let baseline = self.baseline.as_ref().ok_or_else(|| {
            BrowserFailure::new(
                "browser_sandbox_unavailable",
                "Browser worker native launch baseline is unavailable",
            )
        })?;
        Ok(NativeSandboxContext {
            _process_guard: process_guard,
            local_images,
            node_path: &self.node_path,
            chromium_path: &self.chromium_path,
            baseline,
        })
    }

    async fn process_info(
        &self,
        cancel: &dyn Cancellation,
    ) -> Result<WorkerProcessInfo, BrowserFailure> {
        let client = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(BrowserFailure::new("cancelled", "Browser process information query cancelled")),
            client = self.client.lock() => client,
        };
        let client = client
            .as_ref()
            .ok_or_else(|| BrowserFailure::new("worker_closed", "Browser worker is closed"))?;
        let value = client
            .browser_process_info(cancel)
            .await
            .map_err(process_info_failure)?;
        WorkerProcessInfo::decode(value)
    }

    async fn capture_screenshot(&self, cancel: &dyn Cancellation) -> Result<Value, BrowserFailure> {
        let client = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return Err(BrowserFailure::new("cancelled", "Browser screenshot cancelled"));
            }
            client = self.client.lock() => client,
        };
        let client = client
            .as_ref()
            .ok_or_else(|| BrowserFailure::new("worker_closed", "Browser worker is closed"))?;
        client
            .browser_capture_screenshot(cancel)
            .await
            .map_err(capture_screenshot_failure)
    }

    async fn list_tools(&self, cancel: &dyn Cancellation) -> Result<McpCatalog, BrowserFailure> {
        let client = self.client.lock().await;
        let client = client
            .as_ref()
            .ok_or_else(|| BrowserFailure::new("worker_closed", "Browser worker is closed"))?;
        let catalog = client.list_tools(cancel).await.map_err(map_mcp_failure)?;
        self.generation
            .store(catalog.generation, std::sync::atomic::Ordering::Release);
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
            .ok_or_else(|| BrowserFailure::new("worker_closed", "Browser worker is closed"))?;
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
        let path = stage_worker_upload(&self.work_dir, filename, bytes, cancel)?;
        path.to_str().map(str::to_owned).ok_or_else(|| {
            BrowserFailure::new("file_broker", "Browser upload staging path is not UTF-8")
        })
    }

    async fn remove_upload(
        &self,
        path: &str,
        _cancel: &dyn Cancellation,
    ) -> Result<(), BrowserFailure> {
        let uploads = fs::canonicalize(self.work_dir.join("uploads")).map_err(|_| {
            BrowserFailure::new("file_broker", "Browser upload staging is unavailable")
        })?;
        let candidate = match fs::canonicalize(path) {
            Ok(candidate) => candidate,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => {
                return Err(BrowserFailure::new(
                    "file_broker",
                    "Browser upload staging file is unavailable",
                ));
            }
        };
        if !candidate.starts_with(&uploads) {
            return Err(BrowserFailure::new(
                "file_path_denied",
                "Browser upload staging path is outside its directory",
            ));
        }
        match fs::remove_file(&candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(BrowserFailure::new(
                    "file_broker",
                    "Browser upload staging file cannot be removed",
                ));
            }
        }
        // Each upload lives in its own private directory so the page receives
        // the original file name unchanged. Remove that directory as well.
        if let Some(directory) = candidate.parent()
            && directory.parent() == Some(uploads.as_path())
        {
            let _ = fs::remove_dir(directory);
        }
        Ok(())
    }

    async fn read_download(
        &self,
        path: &str,
        max_bytes: usize,
        cancel: &dyn Cancellation,
    ) -> Result<Vec<u8>, BrowserFailure> {
        if cancel.is_cancelled() {
            return Err(BrowserFailure::new(
                "cancelled",
                "Browser download read cancelled",
            ));
        }
        let root = fs::canonicalize(&self.output_dir).map_err(|_| {
            BrowserFailure::new("file_broker", "Browser output directory is unavailable")
        })?;
        let candidate = if path.contains(['/', '\\']) {
            fs::canonicalize(path).map_err(|_| {
                BrowserFailure::new("file_broker", "Browser download file is unavailable")
            })?
        } else {
            fs::canonicalize(root.join(path)).map_err(|_| {
                BrowserFailure::new("file_broker", "Browser download file is unavailable")
            })?
        };
        if !candidate.starts_with(&root) {
            return Err(BrowserFailure::new(
                "file_path_denied",
                "Browser download path is outside the Run output directory",
            ));
        }
        let mut options = std::fs::OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
        let file = options.open(&candidate).map_err(|_| {
            BrowserFailure::new("file_broker", "Browser download file cannot be opened")
        })?;
        let metadata = file.metadata().map_err(|_| {
            BrowserFailure::new("file_broker", "Browser download file cannot be inspected")
        })?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > max_bytes as u64 {
            return Err(BrowserFailure::new(
                "file_too_large",
                "Browser download file is not a bounded regular file",
            ));
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        std::io::Read::read_to_end(&mut &file, &mut bytes).map_err(|_| {
            BrowserFailure::new("file_broker", "Browser download file cannot be read")
        })?;
        if bytes.len() as u64 != metadata.len() || bytes.is_empty() {
            return Err(BrowserFailure::new(
                "file_broker",
                "Browser download file changed while being read",
            ));
        }
        Ok(bytes)
    }

    async fn list_downloads(
        &self,
        cancel: &dyn Cancellation,
    ) -> Result<BTreeSet<String>, BrowserFailure> {
        if cancel.is_cancelled() {
            return Err(BrowserFailure::new(
                "cancelled",
                "Browser download listing cancelled",
            ));
        }
        let root = fs::canonicalize(&self.output_dir).map_err(|_| {
            BrowserFailure::new("file_broker", "Browser output directory is unavailable")
        })?;
        let entries = fs::read_dir(&root).map_err(|_| {
            BrowserFailure::new("file_broker", "Browser output directory cannot be listed")
        })?;
        let mut names = BTreeSet::new();
        for entry in entries {
            let entry = entry.map_err(|_| {
                BrowserFailure::new("file_broker", "Browser output directory cannot be listed")
            })?;
            let file_type = entry.file_type().map_err(|_| {
                BrowserFailure::new("file_broker", "Browser output entry cannot be inspected")
            })?;
            if !file_type.is_file() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if name.starts_with('.')
                || name.contains('\0')
                || name.ends_with(".crdownload")
                || name.ends_with(".part")
                || name.ends_with(".tmp")
                || worker_metadata_file(&name)
            {
                continue;
            }
            names.insert(name);
        }
        Ok(names)
    }

    async fn shutdown(&self) -> Result<(), BrowserFailure> {
        let mut failure: Option<BrowserFailure> = None;
        let client = self.client.lock().await.take();
        if let Some(client) = client
            && client.shutdown().await.is_err()
        {
            failure = Some(BrowserFailure::uncertain(
                "Browser worker MCP shutdown could not be confirmed",
            ));
        }
        let process = self.process.lock().await.take();
        if let Some(mut process) = process
            && process.terminate().await.is_err()
        {
            failure = Some(BrowserFailure::uncertain(
                "Browser worker process termination could not be confirmed",
            ));
        }
        match failure {
            Some(failure) => Err(failure),
            None => Ok(()),
        }
    }
}

/// Production launcher. It composes with `PreparedEnvironment::prepare_command`
/// through `ManagedProcess`, so sandbox selection stays with the host. The
/// isolation environment is carried by each launch request so a fresh
/// environment is used for every worker lifecycle.
pub struct ManagedProcessLauncher {
    pub(super) config: ExecConfig,
}

impl ManagedProcessLauncher {
    pub fn new(config: ExecConfig) -> Self {
        Self { config }
    }
}

#[cfg(target_os = "linux")]
fn map_baseline_probe_error(failure: SandboxProbeFailure) -> BrowserFailure {
    match failure {
        SandboxProbeFailure::PermissionDenied => BrowserFailure::new(
            "browser_sandbox_unavailable",
            "Browser worker baseline namespace verification denied",
        ),
        SandboxProbeFailure::Hard(message) | SandboxProbeFailure::Transient(message) => {
            BrowserFailure::new("browser_sandbox_unavailable", message)
        }
    }
}

#[cfg(target_os = "linux")]
fn capture_linux_supervisor_pin(
    process: &ManagedProcess,
) -> Result<HostProcessPin, BrowserFailure> {
    if process.has_exited().unwrap_or(true) {
        return Err(BrowserFailure::new(
            "browser_sandbox_unavailable",
            "Browser worker supervisor exited immediately after spawn",
        ));
    }
    let supervisor_pid = u32::try_from(process.pid().as_raw()).map_err(|_| {
        BrowserFailure::new(
            "browser_sandbox_unavailable",
            "Browser worker supervisor PID is invalid",
        )
    })?;
    if supervisor_pid == 0 {
        return Err(BrowserFailure::new(
            "browser_sandbox_unavailable",
            "Browser worker supervisor PID is unavailable",
        ));
    }
    let pin = capture_linux_process_pin(Path::new("/proc"), supervisor_pid)
        .map_err(map_baseline_probe_error)?;
    if process.has_exited().unwrap_or(true) {
        return Err(BrowserFailure::new(
            "browser_sandbox_unavailable",
            "Browser worker supervisor exited during baseline pinning",
        ));
    }
    Ok(pin)
}

#[cfg(target_os = "linux")]
fn capture_linux_node_baseline(
    process: &ManagedProcess,
    supervisor_pin: HostProcessPin,
    node_path: &Path,
    pinned_node: &PinnedWorkerImage,
    cancel: &dyn Cancellation,
) -> Result<NativeLaunchBaseline, BrowserFailure> {
    if cancel.is_cancelled() {
        return Err(BrowserFailure::new(
            "cancelled",
            "Browser worker baseline capture cancelled",
        ));
    }
    if process.has_exited().unwrap_or(true) {
        return Err(BrowserFailure::new(
            "browser_sandbox_unavailable",
            "Browser worker supervisor exited before baseline confirmation",
        ));
    }
    let proc_root = Path::new("/proc");
    check_linux_process_pin(proc_root, &supervisor_pin).map_err(map_baseline_probe_error)?;

    let self_pid = std::process::id();
    let self_pid_ns = open_linux_namespace(proc_root, self_pid, KernelNamespaceKind::Pid)
        .map_err(map_baseline_probe_error)?;
    let self_user_ns = open_linux_namespace(proc_root, self_pid, KernelNamespaceKind::User)
        .map_err(map_baseline_probe_error)?;

    let descendants = descendant_pids_strict(&RealKernelProbe, supervisor_pin.pid)
        .map_err(map_baseline_probe_error)?;
    let mut node_matches = Vec::new();
    for descendant in descendants {
        if verify_linux_process_exe(proc_root, descendant, node_path, pinned_node).is_ok() {
            node_matches.push(descendant);
        }
    }
    if node_matches.len() != 1 {
        return Err(BrowserFailure::new(
            "browser_sandbox_unavailable",
            "Browser worker Node process could not be uniquely identified under supervisor",
        ));
    }
    let node_host_pid = node_matches.remove(0);
    let node_pin =
        capture_linux_process_pin(proc_root, node_host_pid).map_err(map_baseline_probe_error)?;
    let node_status =
        read_proc_status_file(&proc_root.join(node_host_pid.to_string()).join("status"))
            .map_err(map_baseline_probe_error)?;
    if node_status.nspid.len() < 2 || node_status.nspid.first().copied() != Some(node_host_pid) {
        return Err(BrowserFailure::new(
            "browser_sandbox_unavailable",
            "Browser worker Node process is not isolated in a child PID namespace",
        ));
    }
    let Some(node_ns_pid) = node_status.nspid.last().copied().filter(|pid| *pid != 0) else {
        return Err(BrowserFailure::new(
            "browser_sandbox_unavailable",
            "Browser worker Node namespace PID is unavailable",
        ));
    };
    if !node_status.no_new_privs || node_status.seccomp_mode != 2 || !node_status.cap_eff_zero {
        return Err(BrowserFailure::new(
            "browser_sandbox_unavailable",
            "Browser worker Node process lacks mandatory native sandbox lockdown",
        ));
    }
    let Some(node_seccomp_filters) = node_status.seccomp_filters.filter(|count| *count >= 1) else {
        return Err(BrowserFailure::new(
            "browser_sandbox_unavailable",
            "Browser worker Node process has no active seccomp filter",
        ));
    };

    let node_pid_ns = open_linux_namespace(proc_root, node_host_pid, KernelNamespaceKind::Pid)
        .map_err(map_baseline_probe_error)?;
    let node_user_ns = open_linux_namespace(proc_root, node_host_pid, KernelNamespaceKind::User)
        .map_err(map_baseline_probe_error)?;
    if node_pid_ns.id == self_pid_ns.id || node_user_ns.id == self_user_ns.id {
        return Err(BrowserFailure::new(
            "browser_sandbox_unavailable",
            "Browser worker Node process did not enter private PID and user namespaces",
        ));
    }
    verify_namespace_descends(
        &RealKernelProbe,
        &node_pid_ns,
        &self_pid_ns.id,
        KernelNamespaceKind::Pid,
    )
    .map_err(map_baseline_probe_error)?;
    verify_namespace_descends(
        &RealKernelProbe,
        &node_user_ns,
        &self_user_ns.id,
        KernelNamespaceKind::User,
    )
    .map_err(map_baseline_probe_error)?;
    let node_pid_owner_user_ns =
        linux_pid_ns_owner_user_ns(&node_pid_ns).map_err(map_baseline_probe_error)?;
    if node_pid_owner_user_ns.id == self_user_ns.id {
        return Err(BrowserFailure::new(
            "browser_sandbox_unavailable",
            "Browser worker Node PID namespace is owned by the host user namespace",
        ));
    }
    verify_namespace_descends(
        &RealKernelProbe,
        &node_pid_owner_user_ns,
        &self_user_ns.id,
        KernelNamespaceKind::User,
    )
    .map_err(map_baseline_probe_error)?;
    if node_user_ns.id != node_pid_owner_user_ns.id {
        verify_namespace_descends(
            &RealKernelProbe,
            &node_user_ns,
            &node_pid_owner_user_ns.id,
            KernelNamespaceKind::User,
        )
        .map_err(map_baseline_probe_error)?;
    }

    verify_linux_process_exe(proc_root, node_host_pid, node_path, pinned_node)
        .map_err(map_baseline_probe_error)?;
    check_linux_process_pin(proc_root, &node_pin).map_err(map_baseline_probe_error)?;
    check_linux_process_pin(proc_root, &supervisor_pin).map_err(map_baseline_probe_error)?;
    if process.has_exited().unwrap_or(true) {
        return Err(BrowserFailure::new(
            "browser_sandbox_unavailable",
            "Browser worker supervisor exited during baseline confirmation",
        ));
    }

    Ok(NativeLaunchBaseline {
        supervisor: supervisor_pin,
        node: NodeLaunchBaseline {
            pin: node_pin,
            ns_pid: node_ns_pid,
            pid_ns: node_pid_ns.id,
            user_ns: node_user_ns.id,
            seccomp_filters: node_seccomp_filters,
        },
    })
}

#[async_trait]
impl WorkerLauncher for ManagedProcessLauncher {
    async fn launch(
        &self,
        request: WorkerLaunchRequest,
        cancel: &dyn Cancellation,
    ) -> Result<Arc<dyn WorkerConnection>, BrowserFailure> {
        if cancel.is_cancelled() {
            return Err(BrowserFailure::new(
                "cancelled",
                "Browser worker launch cancelled before dispatch",
            ));
        }
        let deadline = Instant::now()
            .checked_add(request.mcp_limits.connect_timeout)
            .ok_or_else(|| {
                BrowserFailure::new(
                    "worker_identity",
                    "Browser worker image recheck deadline is out of range",
                )
            })?;
        let mut checkpoint = || {
            if cancel.is_cancelled() {
                return Err(error("cancelled", "Browser worker image recheck cancelled"));
            }
            if Instant::now() >= deadline {
                return Err(identity_error(
                    "Browser worker image recheck exceeded its deadline",
                ));
            }
            Ok(())
        };
        let identity = request.identity.as_ref().ok_or_else(|| {
            BrowserFailure::new(
                "worker_identity",
                "Browser worker launch identity is missing",
            )
        })?;
        identity
            .check_local_executables(&mut checkpoint)
            .map_err(|failure| BrowserFailure::new(failure.0.code, failure.0.message))?;
        let local_images = identity.local_images.clone().ok_or_else(|| {
            BrowserFailure::new(
                "worker_identity",
                "Browser worker local executable identities are unavailable",
            )
        })?;
        let node_path = identity.node.clone();
        let chromium_path = identity.chromium.clone();
        let Some(environment) = request.execution.clone() else {
            return Err(BrowserFailure::new(
                "browser_sandbox_unavailable",
                "Browser worker requires a prepared browser environment",
            ));
        };
        let argv = request
            .argv
            .iter()
            .map(|value| {
                value.to_str().map(str::to_owned).ok_or_else(|| {
                    BrowserFailure::new(
                        "worker_spawn",
                        "Browser worker argv is not representable for the host launcher",
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let plan = crate::tools::exec::ExecPlan {
            argv,
            cwd: request.cwd.clone(),
            timeout: request.timeout,
            environment_digest: environment_digest(&request.env),
        };
        let cwd = {
            let mut options = OpenOptions::new();
            options.read(true).custom_flags(
                libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            );
            let file = options.open(&request.cwd).map_err(|_| {
                BrowserFailure::new(
                    "worker_spawn",
                    "Browser worker work directory is unavailable",
                )
            })?;
            Dir::from_std_file(file)
        };
        let mut config = self.config.clone();
        config.execution = Some(environment);
        let mut process = ManagedProcess::spawn(&plan, cwd, request.env.clone(), &config, true)
            .map_err(|_| {
                BrowserFailure::new(
                    "worker_spawn",
                    "Browser worker process could not be started",
                )
            })?;
        let pid = match u32::try_from(process.pid().as_raw()) {
            Ok(pid) if pid != 0 => pid,
            _ => {
                let _ = process.terminate().await;
                return Err(BrowserFailure::new(
                    "browser_sandbox_unavailable",
                    "Browser worker supervisor PID is unavailable",
                ));
            }
        };
        #[cfg(target_os = "linux")]
        let supervisor_pin = match capture_linux_supervisor_pin(&process) {
            Ok(pin) => pin,
            Err(failure) => {
                if process.terminate().await.is_err() {
                    return Err(BrowserFailure::uncertain(
                        "Browser worker supervisor pinning failed and process cleanup is unknown",
                    ));
                }
                return Err(failure);
            }
        };

        let Some(input) = process.take_stdin() else {
            let _ = process.terminate().await;
            return Err(BrowserFailure::new(
                "worker_spawn",
                "Browser worker stdin pipe is unavailable",
            ));
        };
        let Some(output) = process.take_stdout() else {
            let _ = process.terminate().await;
            return Err(BrowserFailure::new(
                "worker_spawn",
                "Browser worker stdout pipe is unavailable",
            ));
        };
        let stderr_tail = Arc::new(std::sync::Mutex::new(VecDeque::with_capacity(
            request.stderr_limit,
        )));
        if let Some(mut stderr) = process.take_stderr() {
            let keep = request.stderr_limit;
            let shared = stderr_tail.clone();
            tokio::spawn(async move {
                let mut buffer = [0u8; 8192];
                loop {
                    match stderr.read(&mut buffer).await {
                        Ok(0) | Err(_) => break,
                        Ok(count) => {
                            if let Ok(mut tail) = shared.lock() {
                                for byte in &buffer[..count] {
                                    if tail.len() == keep {
                                        tail.pop_front();
                                    }
                                    tail.push_back(*byte);
                                }
                                if tail.capacity() > keep {
                                    tail.shrink_to(keep);
                                }
                            }
                            // The bounded tail is retained for sandbox evidence
                            // and never surfaced to the model.
                        }
                    }
                }
            });
        }

        let client = match crate::mcp::McpClient::connect_stdio(
            "saya-browser-worker",
            output,
            input,
            request.mcp_limits,
            cancel,
        )
        .await
        {
            Ok(client) => client,
            Err(failure) => {
                let code = failure.0.code.clone();
                if process.terminate().await.is_err() {
                    return Err(BrowserFailure::uncertain(
                        "Browser worker initialization failed and process cleanup is unknown",
                    ));
                }
                if code == "cancelled" {
                    return Err(BrowserFailure::new(
                        "cancelled",
                        "Browser worker connect cancelled before initialization completed",
                    ));
                }
                if code == "mcp_protocol_version" {
                    return Err(BrowserFailure::new(
                        "worker_protocol",
                        "Browser worker has no supported protocol version",
                    ));
                }
                return Err(BrowserFailure::new(
                    "worker_connect",
                    "Browser worker initialization failed",
                ));
            }
        };

        #[cfg(target_os = "linux")]
        let baseline = match capture_linux_node_baseline(
            &process,
            supervisor_pin,
            &node_path,
            &local_images.node,
            cancel,
        ) {
            Ok(baseline) => Some(baseline),
            Err(failure) => {
                let _ = client.shutdown().await;
                if process.terminate().await.is_err() {
                    return Err(BrowserFailure::uncertain(
                        "Browser worker baseline capture failed and process cleanup is unknown",
                    ));
                }
                return Err(failure);
            }
        };
        #[cfg(not(target_os = "linux"))]
        let baseline: Option<NativeLaunchBaseline> = None;

        let protocol = client.protocol_version().unwrap_or_default();
        Ok(Arc::new(RealWorkerConnection {
            client: AsyncMutex::new(Some(client)),
            process: Arc::new(AsyncMutex::new(Some(process))),
            protocol,
            generation: std::sync::atomic::AtomicU64::new(0),
            pid,
            stderr_tail,
            work_dir: request.directories.work.clone(),
            output_dir: request.directories.output.clone(),
            local_images: Some(local_images),
            node_path,
            chromium_path,
            baseline,
            #[cfg(test)]
            test_sandbox_ready: false,
        }))
    }
}
