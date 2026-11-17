use super::*;

/// Evidence collected from the real Chromium renderer, not from configuration.
/// It contains process identifiers and sandbox flags only; no page content or
/// credentials are recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChromiumSandboxEvidence {
    pub renderer_pid: u32,
    pub no_new_privs: bool,
    pub seccomp_mode: u32,
    /// Neither `--no-sandbox` nor `--disable-setuid-sandbox` appeared.
    pub sandbox_arguments_valid: bool,
    /// Chromium's own stderr mentioned `InitializeSandbox`.
    pub stderr_initializes_sandbox: bool,
}

/// Host-side verifier for Chromium's own sandbox. Injectable so tests can
/// exercise failure paths without a real browser.
#[async_trait]
pub trait ChromiumSandboxInspector: Send + Sync {
    async fn inspect(
        &self,
        worker_pid: u32,
        stderr_tail: &[u8],
        cancel: &dyn Cancellation,
    ) -> Result<ChromiumSandboxEvidence, BrowserFailure>;

    async fn inspect_connection(
        &self,
        connection: &dyn WorkerConnection,
        cancel: &dyn Cancellation,
    ) -> Result<ChromiumSandboxEvidence, BrowserFailure> {
        let pid = connection
            .process_id()
            .filter(|pid| *pid != 0)
            .ok_or_else(|| {
                BrowserFailure::new(
                    "browser_sandbox_unavailable",
                    "Browser worker process identity is unavailable",
                )
            })?;
        self.inspect(pid, &connection.stderr_tail(), cancel).await
    }
}

pub(super) const MAX_SANDBOX_PROCESSES: usize = 512;
pub(super) const MAX_SANDBOX_PROC_FILE_BYTES: usize = 128 * 1024;
pub(super) const MAX_NAMESPACE_HOPS: usize = 8;

/// Opaque proof that a live native worker connection still owns its launch
/// process, pinned executable identities, and kernel baseline.
pub struct NativeSandboxContext<'a> {
    pub(super) _process_guard: tokio::sync::OwnedMutexGuard<Option<ManagedProcess>>,
    pub(super) local_images: Arc<VerifiedLocalImages>,
    pub(super) node_path: &'a Path,
    pub(super) chromium_path: &'a Path,
    pub(super) baseline: &'a NativeLaunchBaseline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct KernelNamespaceId {
    pub(super) dev: u64,
    pub(super) ino: u64,
}

impl KernelNamespaceId {
    pub(super) fn matches_worker_ns(&self, ns: &WorkerPidNamespace) -> bool {
        let Ok(dev) = ns.dev.parse::<u64>() else {
            return false;
        };
        let Ok(ino) = ns.ino.parse::<u64>() else {
            return false;
        };
        dev != 0 && ino != 0 && self.dev == dev && self.ino == ino
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KernelNamespaceKind {
    Pid,
    User,
}

#[derive(Debug, Clone)]
pub(super) struct VerifiedNamespace {
    pub(super) id: KernelNamespaceId,
    #[cfg(target_os = "linux")]
    pub(super) fd: Option<Arc<OwnedFd>>,
}

#[derive(Debug)]
pub(super) struct HostProcessPin {
    pub(super) pid: u32,
    #[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
    pub(super) starttime: u64,
    #[cfg(target_os = "linux")]
    pub(super) pidfd: Option<OwnedFd>,
}

#[derive(Debug)]
pub(super) struct NodeLaunchBaseline {
    pub(super) pin: HostProcessPin,
    pub(super) ns_pid: u32,
    pub(super) pid_ns: KernelNamespaceId,
    pub(super) user_ns: KernelNamespaceId,
    pub(super) seccomp_filters: u32,
}

#[derive(Debug)]
pub(super) struct NativeLaunchBaseline {
    pub(super) supervisor: HostProcessPin,
    pub(super) node: NodeLaunchBaseline,
}

#[derive(Debug, Clone)]
pub(super) struct KernelProcStatus {
    pub(super) ppid: Option<u32>,
    pub(super) nspid: Vec<u32>,
    pub(super) no_new_privs: bool,
    pub(super) seccomp_mode: u32,
    pub(super) seccomp_filters: Option<u32>,
    pub(super) cap_eff_zero: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SandboxProbeFailure {
    #[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
    PermissionDenied,
    Hard(&'static str),
    Transient(&'static str),
}

pub(super) trait NativeKernelProbe: Send + Sync {
    fn check_supervisor_pin(&self, pin: &HostProcessPin) -> Result<(), SandboxProbeFailure>;
    fn capture_process_pin(&self, pid: u32) -> Result<HostProcessPin, SandboxProbeFailure>;
    fn check_process_pin(&self, pin: &HostProcessPin) -> Result<(), SandboxProbeFailure>;
    fn read_status(&self, pid: u32) -> Result<KernelProcStatus, SandboxProbeFailure>;
    fn read_cmdline(&self, pid: u32) -> Result<Vec<String>, SandboxProbeFailure>;
    fn task_children(&self, pid: u32) -> Result<Vec<u32>, SandboxProbeFailure>;
    fn verify_exe(
        &self,
        pid: u32,
        expected_path: &Path,
        pinned: &PinnedWorkerImage,
    ) -> Result<(), SandboxProbeFailure>;
    fn inspect_namespace(
        &self,
        pid: u32,
        kind: KernelNamespaceKind,
    ) -> Result<VerifiedNamespace, SandboxProbeFailure>;
    fn parent_namespace(
        &self,
        ns: &VerifiedNamespace,
        expected_kind: KernelNamespaceKind,
    ) -> Result<Option<VerifiedNamespace>, SandboxProbeFailure>;
    fn pid_ns_owner_user_ns(
        &self,
        pid_ns: &VerifiedNamespace,
    ) -> Result<VerifiedNamespace, SandboxProbeFailure>;
}

pub(super) struct RealKernelProbe;

impl NativeKernelProbe for RealKernelProbe {
    #[cfg(target_os = "linux")]
    fn check_supervisor_pin(&self, pin: &HostProcessPin) -> Result<(), SandboxProbeFailure> {
        check_linux_process_pin(Path::new("/proc"), pin)
    }

    #[cfg(not(target_os = "linux"))]
    fn check_supervisor_pin(&self, _pin: &HostProcessPin) -> Result<(), SandboxProbeFailure> {
        Err(SandboxProbeFailure::Hard(
            "Native Chromium sandbox verification requires Linux",
        ))
    }

    #[cfg(target_os = "linux")]
    fn capture_process_pin(&self, pid: u32) -> Result<HostProcessPin, SandboxProbeFailure> {
        capture_linux_process_pin(Path::new("/proc"), pid)
    }

    #[cfg(not(target_os = "linux"))]
    fn capture_process_pin(&self, _pid: u32) -> Result<HostProcessPin, SandboxProbeFailure> {
        Err(SandboxProbeFailure::Hard(
            "Native Chromium sandbox verification requires Linux",
        ))
    }

    #[cfg(target_os = "linux")]
    fn check_process_pin(&self, pin: &HostProcessPin) -> Result<(), SandboxProbeFailure> {
        check_linux_process_pin(Path::new("/proc"), pin)
    }

    #[cfg(not(target_os = "linux"))]
    fn check_process_pin(&self, _pin: &HostProcessPin) -> Result<(), SandboxProbeFailure> {
        Err(SandboxProbeFailure::Hard(
            "Native Chromium sandbox verification requires Linux",
        ))
    }

    fn read_status(&self, pid: u32) -> Result<KernelProcStatus, SandboxProbeFailure> {
        read_proc_status_file(&Path::new("/proc").join(pid.to_string()).join("status"))
    }

    fn read_cmdline(&self, pid: u32) -> Result<Vec<String>, SandboxProbeFailure> {
        read_proc_cmdline_file(&Path::new("/proc").join(pid.to_string()).join("cmdline"))
    }

    fn task_children(&self, pid: u32) -> Result<Vec<u32>, SandboxProbeFailure> {
        read_task_children_strict(Path::new("/proc"), pid)
    }

    #[cfg(target_os = "linux")]
    fn verify_exe(
        &self,
        pid: u32,
        expected_path: &Path,
        pinned: &PinnedWorkerImage,
    ) -> Result<(), SandboxProbeFailure> {
        verify_linux_process_exe(Path::new("/proc"), pid, expected_path, pinned)
    }

    #[cfg(not(target_os = "linux"))]
    fn verify_exe(
        &self,
        _pid: u32,
        _expected_path: &Path,
        _pinned: &PinnedWorkerImage,
    ) -> Result<(), SandboxProbeFailure> {
        Err(SandboxProbeFailure::Hard(
            "Native Chromium sandbox verification requires Linux",
        ))
    }

    #[cfg(target_os = "linux")]
    fn inspect_namespace(
        &self,
        pid: u32,
        kind: KernelNamespaceKind,
    ) -> Result<VerifiedNamespace, SandboxProbeFailure> {
        open_linux_namespace(Path::new("/proc"), pid, kind)
    }

    #[cfg(not(target_os = "linux"))]
    fn inspect_namespace(
        &self,
        _pid: u32,
        _kind: KernelNamespaceKind,
    ) -> Result<VerifiedNamespace, SandboxProbeFailure> {
        Err(SandboxProbeFailure::Hard(
            "Native Chromium sandbox verification requires Linux",
        ))
    }

    #[cfg(target_os = "linux")]
    fn parent_namespace(
        &self,
        ns: &VerifiedNamespace,
        expected_kind: KernelNamespaceKind,
    ) -> Result<Option<VerifiedNamespace>, SandboxProbeFailure> {
        linux_parent_namespace(ns, expected_kind)
    }

    #[cfg(not(target_os = "linux"))]
    fn parent_namespace(
        &self,
        _ns: &VerifiedNamespace,
        _expected_kind: KernelNamespaceKind,
    ) -> Result<Option<VerifiedNamespace>, SandboxProbeFailure> {
        Err(SandboxProbeFailure::Hard(
            "Native Chromium sandbox verification requires Linux",
        ))
    }

    #[cfg(target_os = "linux")]
    fn pid_ns_owner_user_ns(
        &self,
        pid_ns: &VerifiedNamespace,
    ) -> Result<VerifiedNamespace, SandboxProbeFailure> {
        linux_pid_ns_owner_user_ns(pid_ns)
    }

    #[cfg(not(target_os = "linux"))]
    fn pid_ns_owner_user_ns(
        &self,
        _pid_ns: &VerifiedNamespace,
    ) -> Result<VerifiedNamespace, SandboxProbeFailure> {
        Err(SandboxProbeFailure::Hard(
            "Native Chromium sandbox verification requires Linux",
        ))
    }
}

struct DeadlineCancel<'a> {
    pub(super) inner: &'a dyn Cancellation,
    pub(super) deadline: Instant,
    pub(super) timed_out: AtomicBool,
}

impl<'a> DeadlineCancel<'a> {
    pub(super) fn new(inner: &'a dyn Cancellation, deadline: Instant) -> Self {
        Self {
            inner,
            deadline,
            timed_out: AtomicBool::new(false),
        }
    }

    pub(super) fn timed_out(&self) -> bool {
        self.timed_out.load(Ordering::Acquire)
    }
}

impl Cancellation for DeadlineCancel<'_> {
    fn is_cancelled(&self) -> bool {
        if self.inner.is_cancelled() {
            return true;
        }
        if Instant::now() >= self.deadline {
            self.timed_out.store(true, Ordering::Release);
            return true;
        }
        false
    }

    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let until = tokio::time::Instant::from_std(self.deadline);
            tokio::select! {
                biased;
                _ = self.inner.cancelled() => {}
                _ = tokio::time::sleep_until(until) => {
                    self.timed_out.store(true, Ordering::Release);
                }
            }
        })
    }
}

async fn run_before_deadline<T, F>(
    fut: F,
    deadline: Instant,
    cancel: &dyn Cancellation,
    deadline_cancel: &DeadlineCancel<'_>,
    cancel_message: &'static str,
    timeout_message: &'static str,
) -> Result<T, BrowserFailure>
where
    F: std::future::Future<Output = Result<T, BrowserFailure>>,
{
    if cancel.is_cancelled() {
        return Err(BrowserFailure::new("cancelled", cancel_message));
    }
    if Instant::now() >= deadline {
        return Err(BrowserFailure::new(
            "browser_sandbox_unavailable",
            timeout_message,
        ));
    }
    let until = tokio::time::Instant::from_std(deadline);
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(BrowserFailure::new("cancelled", cancel_message)),
        outcome = tokio::time::timeout_at(until, fut) => match outcome {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(failure)) => {
                if cancel.is_cancelled() {
                    Err(BrowserFailure::new("cancelled", cancel_message))
                } else if failure.code == "cancelled" && deadline_cancel.timed_out() {
                    Err(BrowserFailure::new("browser_sandbox_unavailable", timeout_message))
                } else {
                    Err(failure)
                }
            }
            Err(_elapsed) => {
                if cancel.is_cancelled() {
                    Err(BrowserFailure::new("cancelled", cancel_message))
                } else {
                    Err(BrowserFailure::new("browser_sandbox_unavailable", timeout_message))
                }
            }
        }
    }
}

/// Host-side verifier for Chromium's own sandbox on native Linux runs.
/// Combines fixed private role queries over the worker MCP pipe with host
/// kernel procfs/pidfd/namespace inspection against the pinned launch baseline.
pub struct ProcfsChromiumSandboxInspector {
    pub(super) probe: Arc<dyn NativeKernelProbe>,
    pub(super) timeout: Duration,
    pub(super) interval: Duration,
}

impl Default for ProcfsChromiumSandboxInspector {
    fn default() -> Self {
        Self {
            probe: Arc::new(RealKernelProbe),
            timeout: Duration::from_secs(5),
            interval: Duration::from_millis(100),
        }
    }
}

impl ProcfsChromiumSandboxInspector {
    #[cfg(test)]
    pub(super) fn with_probe(
        probe: Arc<dyn NativeKernelProbe>,
        timeout: Duration,
        interval: Duration,
    ) -> Self {
        Self {
            probe,
            timeout,
            interval,
        }
    }

    fn map_probe_failure(failure: SandboxProbeFailure) -> Result<BrowserFailure, &'static str> {
        match failure {
            SandboxProbeFailure::PermissionDenied => Ok(BrowserFailure::new(
                "browser_sandbox_unavailable",
                "Chromium sandbox namespace ancestry check returned EPERM",
            )),
            SandboxProbeFailure::Hard(message) => {
                Ok(BrowserFailure::new("browser_sandbox_unavailable", message))
            }
            SandboxProbeFailure::Transient(message) => Err(message),
        }
    }

    fn verify_node_baseline(
        &self,
        context: &NativeSandboxContext<'_>,
    ) -> Result<(), Result<BrowserFailure, &'static str>> {
        self.probe
            .check_supervisor_pin(&context.baseline.supervisor)
            .map_err(Self::map_probe_failure)?;
        self.probe
            .check_process_pin(&context.baseline.node.pin)
            .map_err(Self::map_probe_failure)?;
        self.probe
            .verify_exe(
                context.baseline.node.pin.pid,
                context.node_path,
                &context.local_images.node,
            )
            .map_err(Self::map_probe_failure)?;
        let node_status = self
            .probe
            .read_status(context.baseline.node.pin.pid)
            .map_err(Self::map_probe_failure)?;
        if node_status.nspid.first().copied() != Some(context.baseline.node.pin.pid)
            || node_status.nspid.last().copied() != Some(context.baseline.node.ns_pid)
        {
            return Err(Ok(BrowserFailure::new(
                "browser_sandbox_unavailable",
                "Browser worker node namespace PID no longer matches baseline",
            )));
        }
        if !node_status.no_new_privs
            || node_status.seccomp_mode != 2
            || node_status.seccomp_filters != Some(context.baseline.node.seccomp_filters)
            || !node_status.cap_eff_zero
        {
            return Err(Ok(BrowserFailure::new(
                "browser_sandbox_unavailable",
                "Browser worker node kernel sandbox baseline changed after launch",
            )));
        }
        let node_pid_ns = self
            .probe
            .inspect_namespace(context.baseline.node.pin.pid, KernelNamespaceKind::Pid)
            .map_err(Self::map_probe_failure)?;
        let node_user_ns = self
            .probe
            .inspect_namespace(context.baseline.node.pin.pid, KernelNamespaceKind::User)
            .map_err(Self::map_probe_failure)?;
        if node_pid_ns.id != context.baseline.node.pid_ns
            || node_user_ns.id != context.baseline.node.user_ns
        {
            return Err(Ok(BrowserFailure::new(
                "browser_sandbox_unavailable",
                "Browser worker node namespace changed after launch",
            )));
        }
        let supervisor_descendants =
            descendant_pids_strict(self.probe.as_ref(), context.baseline.supervisor.pid)
                .map_err(Self::map_probe_failure)?;
        if !supervisor_descendants.contains(&context.baseline.node.pin.pid) {
            return Err(Ok(BrowserFailure::new(
                "browser_sandbox_unavailable",
                "Browser worker node process is no longer under its sandbox supervisor",
            )));
        }
        for descendant in &supervisor_descendants {
            match self.probe.read_cmdline(*descendant) {
                Ok(tokens) => {
                    let tokens = normalize_cmdline_tokens(tokens);
                    if has_forbidden_sandbox_flag(&tokens) {
                        return Err(Ok(BrowserFailure::new(
                            "browser_sandbox_unavailable",
                            "Browser worker process tree contains a sandbox-disabling argument",
                        )));
                    }
                }
                Err(SandboxProbeFailure::Transient(_)) => {}
                Err(failure) => return Err(Self::map_probe_failure(failure)),
            }
        }
        Ok(())
    }

    fn verify_browser_process(
        &self,
        context: &NativeSandboxContext<'_>,
        browser_ns_pid: u32,
    ) -> Result<
        (HostProcessPin, VerifiedNamespace, VerifiedNamespace, u32),
        Result<BrowserFailure, &'static str>,
    > {
        let node_descendants =
            descendant_pids_strict(self.probe.as_ref(), context.baseline.node.pin.pid)
                .map_err(Self::map_probe_failure)?;
        let mut browser_candidates = Vec::new();
        for descendant in &node_descendants {
            let status = match self.probe.read_status(*descendant) {
                Ok(status) => status,
                Err(SandboxProbeFailure::Transient(_)) => continue,
                Err(failure) => return Err(Self::map_probe_failure(failure)),
            };
            if status.nspid.last().copied() != Some(browser_ns_pid) {
                continue;
            }
            let pid_ns = match self
                .probe
                .inspect_namespace(*descendant, KernelNamespaceKind::Pid)
            {
                Ok(ns) => ns,
                Err(SandboxProbeFailure::Transient(_)) => continue,
                Err(failure) => return Err(Self::map_probe_failure(failure)),
            };
            if pid_ns.id == context.baseline.node.pid_ns {
                browser_candidates.push((*descendant, pid_ns, status));
            }
        }
        if browser_candidates.is_empty() {
            return Err(Err("Chromium browser process was not found under worker"));
        }
        if browser_candidates.len() > 1 {
            return Err(Ok(BrowserFailure::new(
                "browser_sandbox_unavailable",
                "Chromium browser process identity is ambiguous",
            )));
        }
        let (browser_host_pid, browser_pid_ns, browser_status) = browser_candidates.remove(0);
        let browser_pin = self
            .probe
            .capture_process_pin(browser_host_pid)
            .map_err(Self::map_probe_failure)?;
        self.probe
            .verify_exe(
                browser_host_pid,
                context.chromium_path,
                &context.local_images.chromium,
            )
            .map_err(Self::map_probe_failure)?;
        let browser_tokens = normalize_cmdline_tokens(
            self.probe
                .read_cmdline(browser_host_pid)
                .map_err(Self::map_probe_failure)?,
        );
        if browser_tokens.is_empty()
            || browser_tokens
                .iter()
                .any(|token| token == "--type" || token.starts_with("--type="))
            || has_forbidden_sandbox_flag(&browser_tokens)
        {
            return Err(Ok(BrowserFailure::new(
                "browser_sandbox_unavailable",
                "Chromium browser process command line is invalid or disables its sandbox",
            )));
        }
        let browser_user_ns = self
            .probe
            .inspect_namespace(browser_host_pid, KernelNamespaceKind::User)
            .map_err(Self::map_probe_failure)?;
        if browser_user_ns.id != context.baseline.node.user_ns {
            return Err(Ok(BrowserFailure::new(
                "browser_sandbox_unavailable",
                "Chromium browser process user namespace disagrees with worker node",
            )));
        }
        let Some(browser_seccomp_filters) = browser_status
            .seccomp_filters
            .filter(|&filters| filters >= context.baseline.node.seccomp_filters)
        else {
            return Err(Ok(BrowserFailure::new(
                "browser_sandbox_unavailable",
                "Chromium browser process seccomp filters are weaker than worker baseline",
            )));
        };
        if !browser_status.no_new_privs
            || browser_status.seccomp_mode != 2
            || !browser_status.cap_eff_zero
        {
            return Err(Ok(BrowserFailure::new(
                "browser_sandbox_unavailable",
                "Chromium browser process lacks mandatory kernel sandbox attributes",
            )));
        }
        Ok((
            browser_pin,
            browser_pid_ns,
            browser_user_ns,
            browser_seccomp_filters,
        ))
    }

    fn verify_renderer_processes(
        &self,
        context: &NativeSandboxContext<'_>,
        browser_host_pid: u32,
        browser_pid_ns: &VerifiedNamespace,
        browser_user_ns: &VerifiedNamespace,
        browser_seccomp_filters: u32,
        expected_renderer_pids: &[u32],
    ) -> Result<Vec<HostProcessPin>, Result<BrowserFailure, &'static str>> {
        let browser_descendants = descendant_pids_strict(self.probe.as_ref(), browser_host_pid)
            .map_err(Self::map_probe_failure)?;
        let mut renderer_pids = Vec::new();
        for descendant in browser_descendants {
            let tokens = match self.probe.read_cmdline(descendant) {
                Ok(tokens) => normalize_cmdline_tokens(tokens),
                Err(SandboxProbeFailure::Transient(_)) => continue,
                Err(failure) => return Err(Self::map_probe_failure(failure)),
            };
            if has_forbidden_sandbox_flag(&tokens) {
                return Err(Ok(BrowserFailure::new(
                    "browser_sandbox_unavailable",
                    "Chromium descendant command line disables its sandbox",
                )));
            }
            if tokens.iter().any(|token| token == "--type=renderer") {
                renderer_pids.push((descendant, tokens));
            }
        }
        if renderer_pids.is_empty() {
            return Err(Err("Chromium renderer process is not active yet"));
        }

        let expected_renderers: BTreeSet<u32> = expected_renderer_pids.iter().copied().collect();
        let mut observed_renderers = BTreeSet::new();
        let mut renderer_pins = Vec::with_capacity(renderer_pids.len());

        for (r_host_pid, _tokens) in renderer_pids {
            let r_pin = self
                .probe
                .capture_process_pin(r_host_pid)
                .map_err(Self::map_probe_failure)?;
            let r_status = self
                .probe
                .read_status(r_host_pid)
                .map_err(Self::map_probe_failure)?;
            self.probe
                .verify_exe(
                    r_host_pid,
                    context.chromium_path,
                    &context.local_images.chromium,
                )
                .map_err(Self::map_probe_failure)?;
            let r_pid_ns = self
                .probe
                .inspect_namespace(r_host_pid, KernelNamespaceKind::Pid)
                .map_err(Self::map_probe_failure)?;
            if r_pid_ns.id == browser_pid_ns.id {
                return Err(Ok(BrowserFailure::new(
                    "browser_sandbox_unavailable",
                    "Chromium renderer shares its PID namespace with the browser process",
                )));
            }
            let pid_hops = verify_namespace_descends(
                self.probe.as_ref(),
                &r_pid_ns,
                &browser_pid_ns.id,
                KernelNamespaceKind::Pid,
            )
            .map_err(Self::map_probe_failure)?;
            if r_status.nspid.first().copied() != Some(r_host_pid)
                || r_status.nspid.len() <= pid_hops
            {
                return Err(Ok(BrowserFailure::new(
                    "browser_sandbox_unavailable",
                    "Chromium renderer NSpid depth disagrees with kernel PID namespace ancestry",
                )));
            }
            let r_ns_pid = r_status.nspid[r_status.nspid.len() - 1 - pid_hops];
            if r_ns_pid == 0 || !observed_renderers.insert(r_ns_pid) {
                return Err(Ok(BrowserFailure::new(
                    "browser_sandbox_unavailable",
                    "Chromium renderer namespace PID is invalid or duplicated",
                )));
            }

            let r_user_ns = self
                .probe
                .inspect_namespace(r_host_pid, KernelNamespaceKind::User)
                .map_err(Self::map_probe_failure)?;
            if r_user_ns.id == browser_user_ns.id {
                return Err(Ok(BrowserFailure::new(
                    "browser_sandbox_unavailable",
                    "Chromium renderer shares its user namespace with the browser process",
                )));
            }
            verify_namespace_descends(
                self.probe.as_ref(),
                &r_user_ns,
                &browser_user_ns.id,
                KernelNamespaceKind::User,
            )
            .map_err(Self::map_probe_failure)?;

            let r_pid_owner_user_ns = self
                .probe
                .pid_ns_owner_user_ns(&r_pid_ns)
                .map_err(Self::map_probe_failure)?;
            if r_pid_owner_user_ns.id == browser_user_ns.id {
                return Err(Ok(BrowserFailure::new(
                    "browser_sandbox_unavailable",
                    "Chromium renderer PID namespace is owned by the browser user namespace",
                )));
            }
            verify_namespace_descends(
                self.probe.as_ref(),
                &r_pid_owner_user_ns,
                &browser_user_ns.id,
                KernelNamespaceKind::User,
            )
            .map_err(Self::map_probe_failure)?;

            if !r_status.no_new_privs
                || r_status.seccomp_mode != 2
                || !r_status.seccomp_filters.is_some_and(|filters| {
                    filters > context.baseline.node.seccomp_filters
                        && filters > browser_seccomp_filters
                })
                || !r_status.cap_eff_zero
            {
                return Err(Err(
                    "Chromium renderer kernel sandbox protections are incomplete",
                ));
            }
            renderer_pins.push(r_pin);
        }

        if observed_renderers != expected_renderers {
            return Err(Err(
                "Chromium renderer inventory disagrees with kernel process tree",
            ));
        }
        Ok(renderer_pins)
    }

    fn recheck_verified_pins(
        &self,
        context: &NativeSandboxContext<'_>,
        browser_pin: &HostProcessPin,
        renderer_pins: &[HostProcessPin],
    ) -> Result<(), Result<BrowserFailure, &'static str>> {
        self.probe
            .check_supervisor_pin(&context.baseline.supervisor)
            .map_err(Self::map_probe_failure)?;
        self.probe
            .check_process_pin(&context.baseline.node.pin)
            .map_err(Self::map_probe_failure)?;
        self.probe
            .verify_exe(
                context.baseline.node.pin.pid,
                context.node_path,
                &context.local_images.node,
            )
            .map_err(Self::map_probe_failure)?;
        self.probe
            .check_process_pin(browser_pin)
            .map_err(Self::map_probe_failure)?;
        self.probe
            .verify_exe(
                browser_pin.pid,
                context.chromium_path,
                &context.local_images.chromium,
            )
            .map_err(Self::map_probe_failure)?;
        for r_pin in renderer_pins {
            self.probe
                .check_process_pin(r_pin)
                .map_err(Self::map_probe_failure)?;
            self.probe
                .verify_exe(
                    r_pin.pid,
                    context.chromium_path,
                    &context.local_images.chromium,
                )
                .map_err(Self::map_probe_failure)?;
        }
        Ok(())
    }

    async fn verify_once(
        &self,
        connection: &dyn WorkerConnection,
        context: &NativeSandboxContext<'_>,
        cancel: &dyn Cancellation,
        deadline_cancel: &DeadlineCancel<'_>,
        deadline: Instant,
    ) -> Result<ChromiumSandboxEvidence, Result<BrowserFailure, &'static str>> {
        self.verify_node_baseline(context)?;

        let q1 = run_before_deadline(
            connection.process_info(deadline_cancel),
            deadline,
            cancel,
            deadline_cancel,
            "Chromium sandbox verification cancelled",
            "Chromium sandbox verification timed out querying worker process roles",
        )
        .await
        .map_err(Ok)?;
        if q1.node_pid != context.baseline.node.ns_pid
            || !context
                .baseline
                .node
                .pid_ns
                .matches_worker_ns(&q1.node_pid_namespace)
            || q1.renderer_pids.is_empty()
        {
            return Err(Ok(BrowserFailure::new(
                "browser_sandbox_unavailable",
                "Browser worker reported inconsistent or empty Chromium roles",
            )));
        }

        let (browser_pin, browser_pid_ns, browser_user_ns, browser_seccomp_filters) =
            self.verify_browser_process(context, q1.browser_pid)?;
        let renderer_pins = self.verify_renderer_processes(
            context,
            browser_pin.pid,
            &browser_pid_ns,
            &browser_user_ns,
            browser_seccomp_filters,
            &q1.renderer_pids,
        )?;

        let q2 = run_before_deadline(
            connection.process_info(deadline_cancel),
            deadline,
            cancel,
            deadline_cancel,
            "Chromium sandbox verification cancelled",
            "Chromium sandbox verification timed out rechecking worker process roles",
        )
        .await
        .map_err(Ok)?;
        if q2 != q1 {
            return Err(Ok(BrowserFailure::new(
                "browser_sandbox_unavailable",
                "Browser worker process roles changed during sandbox verification",
            )));
        }

        self.recheck_verified_pins(context, &browser_pin, &renderer_pins)?;

        Ok(ChromiumSandboxEvidence {
            renderer_pid: q1.renderer_pids[0],
            no_new_privs: true,
            seccomp_mode: 2,
            sandbox_arguments_valid: true,
            stderr_initializes_sandbox: true,
        })
    }
}

#[async_trait]
impl ChromiumSandboxInspector for ProcfsChromiumSandboxInspector {
    async fn inspect(
        &self,
        _worker_pid: u32,
        _stderr_tail: &[u8],
        cancel: &dyn Cancellation,
    ) -> Result<ChromiumSandboxEvidence, BrowserFailure> {
        if cancel.is_cancelled() {
            return Err(BrowserFailure::new(
                "cancelled",
                "Chromium sandbox verification cancelled",
            ));
        }
        Err(BrowserFailure::new(
            "browser_sandbox_unavailable",
            "Native Chromium sandbox verification requires a live verified worker connection",
        ))
    }

    async fn inspect_connection(
        &self,
        connection: &dyn WorkerConnection,
        cancel: &dyn Cancellation,
    ) -> Result<ChromiumSandboxEvidence, BrowserFailure> {
        if cancel.is_cancelled() {
            return Err(BrowserFailure::new(
                "cancelled",
                "Chromium sandbox verification cancelled",
            ));
        }
        let deadline = Instant::now().checked_add(self.timeout).ok_or_else(|| {
            BrowserFailure::new(
                "browser_sandbox_unavailable",
                "Chromium sandbox verification deadline is out of range",
            )
        })?;
        let deadline_cancel = DeadlineCancel::new(cancel, deadline);
        let context = run_before_deadline(
            connection.native_sandbox_context(&deadline_cancel),
            deadline,
            cancel,
            &deadline_cancel,
            "Chromium sandbox verification cancelled",
            "Chromium sandbox verification timed out waiting for worker context",
        )
        .await?;

        loop {
            if cancel.is_cancelled() {
                drop(context);
                return Err(BrowserFailure::new(
                    "cancelled",
                    "Chromium sandbox verification cancelled",
                ));
            }
            let message = match self
                .verify_once(connection, &context, cancel, &deadline_cancel, deadline)
                .await
            {
                Ok(evidence) => {
                    drop(context);
                    return Ok(evidence);
                }
                Err(Ok(failure)) => {
                    drop(context);
                    return Err(failure);
                }
                Err(Err(message)) => message,
            };
            let now = Instant::now();
            if now >= deadline {
                drop(context);
                return Err(BrowserFailure::new("browser_sandbox_unavailable", message));
            }
            let wait = self.interval.min(deadline.saturating_duration_since(now));
            if wait.is_zero() {
                drop(context);
                return Err(BrowserFailure::new("browser_sandbox_unavailable", message));
            }
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    drop(context);
                    return Err(BrowserFailure::new(
                        "cancelled",
                        "Chromium sandbox verification cancelled",
                    ));
                }
                _ = tokio::time::sleep(wait) => {}
            }
            if Instant::now() >= deadline {
                drop(context);
                return Err(BrowserFailure::new("browser_sandbox_unavailable", message));
            }
        }
    }
}

pub(super) fn verify_namespace_descends(
    probe: &dyn NativeKernelProbe,
    start: &VerifiedNamespace,
    ancestor: &KernelNamespaceId,
    kind: KernelNamespaceKind,
) -> Result<usize, SandboxProbeFailure> {
    let mut hops = 0usize;
    let mut current = start.clone();
    loop {
        hops += 1;
        if hops > MAX_NAMESPACE_HOPS {
            return Err(SandboxProbeFailure::Hard(
                "Chromium namespace ancestry exceeds maximum nesting depth",
            ));
        }
        let Some(parent) = probe.parent_namespace(&current, kind)? else {
            return Err(SandboxProbeFailure::Hard(
                "Chromium namespace does not descend from browser baseline namespace",
            ));
        };
        if &parent.id == ancestor {
            return Ok(hops);
        }
        current = parent;
    }
}

fn has_forbidden_sandbox_flag(tokens: &[String]) -> bool {
    tokens
        .iter()
        .flat_map(|token| token.split_whitespace())
        .any(|token| {
            matches!(token, "--no-sandbox" | "--disable-setuid-sandbox")
                || token.starts_with("--no-sandbox=")
                || token.starts_with("--disable-setuid-sandbox=")
        })
}

pub(super) fn read_bounded_proc_file_strict(path: &Path) -> Result<Vec<u8>, SandboxProbeFailure> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    let mut file = options.open(path).map_err(|err| {
        if err.kind() == io::ErrorKind::NotFound || err.raw_os_error() == Some(libc::ESRCH) {
            SandboxProbeFailure::Transient("Browser procfs entry disappeared")
        } else {
            SandboxProbeFailure::Hard("Browser procfs entry cannot be opened")
        }
    })?;
    let mut bytes = Vec::new();
    let limit = (MAX_SANDBOX_PROC_FILE_BYTES + 1) as u64;
    std::io::Read::by_ref(&mut file)
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(|err| {
            if err.raw_os_error() == Some(libc::ESRCH) {
                SandboxProbeFailure::Transient("Browser process exited while reading procfs")
            } else {
                SandboxProbeFailure::Hard("Browser procfs entry cannot be read")
            }
        })?;
    if bytes.len() > MAX_SANDBOX_PROC_FILE_BYTES {
        return Err(SandboxProbeFailure::Hard(
            "Browser procfs entry exceeds size limit",
        ));
    }
    Ok(bytes)
}

fn parse_proc_status(bytes: &[u8]) -> Result<KernelProcStatus, SandboxProbeFailure> {
    let text = String::from_utf8_lossy(bytes);
    let mut ppid = None;
    let mut nspid = Vec::new();
    let mut no_new_privs = false;
    let mut seccomp_mode = 0;
    let mut seccomp_filters = None;
    let mut cap_eff_zero = false;
    let mut saw_nspid = false;
    let mut saw_cap_eff = false;

    for line in text.lines() {
        if let Some(value) = line.strip_prefix("PPid:") {
            ppid = Some(
                value
                    .trim()
                    .parse::<u32>()
                    .map_err(|_| SandboxProbeFailure::Hard("Browser procfs PPid is invalid"))?,
            );
        } else if let Some(value) = line.strip_prefix("NSpid:") {
            if saw_nspid {
                return Err(SandboxProbeFailure::Hard(
                    "Browser procfs contains duplicate NSpid",
                ));
            }
            saw_nspid = true;
            for token in value.split_whitespace() {
                let parsed = token
                    .parse::<u32>()
                    .map_err(|_| SandboxProbeFailure::Hard("Browser procfs NSpid is invalid"))?;
                if parsed == 0 {
                    return Err(SandboxProbeFailure::Hard(
                        "Browser procfs NSpid cannot contain 0",
                    ));
                }
                nspid.push(parsed);
            }
        } else if let Some(value) = line.strip_prefix("NoNewPrivs:") {
            no_new_privs = value.trim() == "1";
        } else if let Some(value) = line.strip_prefix("Seccomp:") {
            seccomp_mode = value
                .trim()
                .parse::<u32>()
                .map_err(|_| SandboxProbeFailure::Hard("Browser procfs Seccomp is invalid"))?;
        } else if let Some(value) = line.strip_prefix("Seccomp_filters:") {
            seccomp_filters = Some(value.trim().parse::<u32>().map_err(|_| {
                SandboxProbeFailure::Hard("Browser procfs Seccomp_filters is invalid")
            })?);
        } else if let Some(value) = line.strip_prefix("CapEff:") {
            if saw_cap_eff {
                return Err(SandboxProbeFailure::Hard(
                    "Browser procfs contains duplicate CapEff",
                ));
            }
            saw_cap_eff = true;
            let trimmed = value.trim();
            if trimmed.is_empty() || !trimmed.chars().all(|ch| ch.is_ascii_hexdigit()) {
                return Err(SandboxProbeFailure::Hard(
                    "Browser procfs CapEff is malformed",
                ));
            }
            cap_eff_zero = trimmed.chars().all(|ch| ch == '0');
        }
    }

    Ok(KernelProcStatus {
        ppid,
        nspid,
        no_new_privs,
        seccomp_mode,
        seccomp_filters,
        cap_eff_zero,
    })
}

pub(super) fn read_proc_status_file(path: &Path) -> Result<KernelProcStatus, SandboxProbeFailure> {
    let bytes = read_bounded_proc_file_strict(path)?;
    parse_proc_status(&bytes)
}

fn normalize_cmdline_tokens(tokens: Vec<String>) -> Vec<String> {
    // Chromium's Linux zygote/renderer calls `setproctitle`, collapsing argv
    // into one NUL-terminated ASCII-whitespace-separated string in `/proc/<pid>/cmdline`.
    if tokens.len() == 1 && tokens[0].contains(char::is_whitespace) {
        tokens[0].split_whitespace().map(str::to_owned).collect()
    } else {
        tokens
    }
}

pub(super) fn parse_cmdline_tokens(bytes: &[u8]) -> Vec<String> {
    let tokens: Vec<String> = bytes
        .split(|byte| *byte == 0)
        .filter(|token| !token.is_empty())
        .map(|token| String::from_utf8_lossy(token).into_owned())
        .collect();
    normalize_cmdline_tokens(tokens)
}

fn read_proc_cmdline_file(path: &Path) -> Result<Vec<String>, SandboxProbeFailure> {
    let bytes = read_bounded_proc_file_strict(path)?;
    Ok(parse_cmdline_tokens(&bytes))
}

#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
pub(super) fn parse_proc_stat_starttime(
    bytes: &[u8],
    expected_pid: u32,
) -> Result<u64, SandboxProbeFailure> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| SandboxProbeFailure::Hard("Browser procfs stat is not UTF-8"))?;
    let lparen = text.find('(').ok_or(SandboxProbeFailure::Hard(
        "Browser procfs stat is malformed",
    ))?;
    let rparen = text.rfind(')').ok_or(SandboxProbeFailure::Hard(
        "Browser procfs stat is malformed",
    ))?;
    if lparen >= rparen {
        return Err(SandboxProbeFailure::Hard(
            "Browser procfs stat is malformed",
        ));
    }
    let stat_pid = text[..lparen]
        .trim()
        .parse::<u32>()
        .map_err(|_| SandboxProbeFailure::Hard("Browser procfs stat PID is invalid"))?;
    if stat_pid != expected_pid {
        return Err(SandboxProbeFailure::Hard(
            "Browser procfs stat PID mismatch",
        ));
    }
    let mut fields = text[rparen + 1..].split_whitespace();
    let state = fields
        .next()
        .ok_or(SandboxProbeFailure::Hard("Browser procfs state is missing"))?;
    if matches!(state, "Z" | "X" | "x") {
        return Err(SandboxProbeFailure::Hard(
            "Browser worker process is dead or zombie",
        ));
    }
    let starttime_raw = fields.nth(18).ok_or(SandboxProbeFailure::Hard(
        "Browser procfs starttime is missing",
    ))?;
    starttime_raw
        .parse::<u64>()
        .map_err(|_| SandboxProbeFailure::Hard("Browser procfs starttime is invalid"))
}

fn read_task_children_strict(proc_root: &Path, pid: u32) -> Result<Vec<u32>, SandboxProbeFailure> {
    let task_root = proc_root.join(pid.to_string()).join("task");
    let tasks = fs::read_dir(&task_root).map_err(|err| {
        if err.kind() == io::ErrorKind::NotFound || err.raw_os_error() == Some(libc::ESRCH) {
            SandboxProbeFailure::Transient("Browser task directory disappeared")
        } else {
            SandboxProbeFailure::Hard("Browser task directory cannot be read")
        }
    })?;
    let mut children = Vec::new();
    for entry in tasks {
        let entry = entry.map_err(|err| {
            if err.kind() == io::ErrorKind::NotFound || err.raw_os_error() == Some(libc::ESRCH) {
                SandboxProbeFailure::Transient("Browser task entry disappeared")
            } else {
                SandboxProbeFailure::Hard("Browser task entry cannot be read")
            }
        })?;
        let path = entry.path().join("children");
        let bytes = match read_bounded_proc_file_strict(&path) {
            Ok(bytes) => bytes,
            Err(SandboxProbeFailure::Transient(_)) => continue,
            Err(failure) => return Err(failure),
        };
        for token in String::from_utf8_lossy(&bytes).split_whitespace() {
            let child = token
                .parse::<u32>()
                .map_err(|_| SandboxProbeFailure::Hard("Browser children entry is invalid"))?;
            if child != 0 {
                children.push(child);
            }
        }
    }
    children.sort_unstable();
    children.dedup();
    Ok(children)
}

pub(super) fn descendant_pids_strict(
    probe: &dyn NativeKernelProbe,
    root: u32,
) -> Result<Vec<u32>, SandboxProbeFailure> {
    let mut queue = VecDeque::from([root]);
    let mut seen = BTreeSet::from([root]);
    let mut descendants = Vec::new();
    while let Some(parent) = queue.pop_front() {
        let children = match probe.task_children(parent) {
            Ok(children) => children,
            Err(SandboxProbeFailure::Transient(_)) if parent != root => continue,
            Err(failure) => return Err(failure),
        };
        for child in children {
            if !seen.insert(child) {
                continue;
            }
            let status = match probe.read_status(child) {
                Ok(status) => status,
                Err(SandboxProbeFailure::Transient(_)) => continue,
                Err(failure) => return Err(failure),
            };
            if status.ppid != Some(parent) {
                continue;
            }
            if descendants.len() >= MAX_SANDBOX_PROCESSES {
                return Err(SandboxProbeFailure::Hard(
                    "Browser process tree exceeds maximum process limit",
                ));
            }
            descendants.push(child);
            queue.push_back(child);
        }
    }
    Ok(descendants)
}

#[cfg(target_os = "linux")]
fn open_linux_pidfd(pid: u32) -> Result<OwnedFd, SandboxProbeFailure> {
    let pid_t = libc::pid_t::try_from(pid)
        .map_err(|_| SandboxProbeFailure::Hard("Browser worker PID is out of range"))?;
    // SAFETY: `pidfd_open` is invoked with a validated PID and `PIDFD_NONBLOCK`.
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid_t, libc::PIDFD_NONBLOCK) };
    if raw < 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            return Err(SandboxProbeFailure::Hard(
                "Browser process exited before pidfd pinning",
            ));
        }
        return Err(SandboxProbeFailure::Hard(
            "Browser process pidfd could not be opened",
        ));
    }
    // SAFETY: `pidfd_open` succeeded and returned a fresh owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(raw as RawFd) })
}

#[cfg(target_os = "linux")]
fn linux_pidfd_is_alive(pidfd: &BorrowedFd<'_>) -> bool {
    let mut pfd = libc::pollfd {
        fd: pidfd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        // SAFETY: `pfd` points to valid writable storage for a single `pollfd`.
        let rc = unsafe { libc::poll(&mut pfd as *mut libc::pollfd, 1, 0) };
        if rc < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return false;
        }
        return rc == 0
            && (pfd.revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP | libc::POLLNVAL))
                == 0;
    }
}

#[cfg(target_os = "linux")]
pub(super) fn capture_linux_process_pin(
    proc_root: &Path,
    pid: u32,
) -> Result<HostProcessPin, SandboxProbeFailure> {
    let stat_path = proc_root.join(pid.to_string()).join("stat");
    let before = parse_proc_stat_starttime(&read_bounded_proc_file_strict(&stat_path)?, pid)?;
    let pidfd = open_linux_pidfd(pid)?;
    if !linux_pidfd_is_alive(&unsafe { BorrowedFd::borrow_raw(pidfd.as_raw_fd()) }) {
        return Err(SandboxProbeFailure::Hard(
            "Browser process exited before baseline confirmation",
        ));
    }
    let after = parse_proc_stat_starttime(&read_bounded_proc_file_strict(&stat_path)?, pid)?;
    if before != after {
        return Err(SandboxProbeFailure::Hard(
            "Browser process PID was reused during pinning",
        ));
    }
    Ok(HostProcessPin {
        pid,
        starttime: before,
        pidfd: Some(pidfd),
    })
}

#[cfg(target_os = "linux")]
pub(super) fn check_linux_process_pin(
    proc_root: &Path,
    pin: &HostProcessPin,
) -> Result<(), SandboxProbeFailure> {
    let Some(pidfd) = pin.pidfd.as_ref() else {
        return Err(SandboxProbeFailure::Hard(
            "Browser process pidfd baseline is unavailable",
        ));
    };
    // SAFETY: `pidfd` stays alive across the synchronous `poll` call.
    let borrowed = unsafe { BorrowedFd::borrow_raw(pidfd.as_raw_fd()) };
    if !linux_pidfd_is_alive(&borrowed) {
        return Err(SandboxProbeFailure::Hard(
            "Browser process exited during verification",
        ));
    }
    let stat_path = proc_root.join(pin.pid.to_string()).join("stat");
    let current = parse_proc_stat_starttime(&read_bounded_proc_file_strict(&stat_path)?, pin.pid)?;
    if current != pin.starttime {
        return Err(SandboxProbeFailure::Hard(
            "Browser process start time changed after pinning",
        ));
    }
    if !linux_pidfd_is_alive(&borrowed) {
        return Err(SandboxProbeFailure::Hard(
            "Browser process exited during start time confirmation",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn verify_linux_process_exe(
    proc_root: &Path,
    pid: u32,
    expected_path: &Path,
    pinned: &PinnedWorkerImage,
) -> Result<(), SandboxProbeFailure> {
    let named_before = open_absolute_file(expected_path).map_err(|_| {
        SandboxProbeFailure::Hard("Browser worker named executable cannot be opened")
    })?;
    pinned.check_metadata(&named_before).map_err(|_| {
        SandboxProbeFailure::Hard(
            "Browser worker named executable metadata does not match pinned binary",
        )
    })?;
    let exe_path = proc_root.join(pid.to_string()).join("exe");
    let link_before = fs::read_link(&exe_path)
        .map_err(|_| SandboxProbeFailure::Hard("Browser process exe link is unreadable"))?;
    if link_before.as_os_str().as_bytes().ends_with(b" (deleted)") {
        return Err(SandboxProbeFailure::Hard(
            "Browser process executable has been unlinked or replaced",
        ));
    }
    let canonical_expected = fs::canonicalize(expected_path);
    if link_before != expected_path
        && canonical_expected
            .as_ref()
            .ok()
            .is_none_or(|canonical| &link_before != canonical)
    {
        return Err(SandboxProbeFailure::Hard(
            "Browser process executable path does not match pinned binary",
        ));
    }
    let mut options = OpenOptions::new();
    // Magic procfs `/proc/<pid>/exe` symlinks cannot be opened with `O_NOFOLLOW`.
    options
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK);
    let file = options
        .open(&exe_path)
        .map_err(|_| SandboxProbeFailure::Hard("Browser process executable cannot be opened"))?;
    pinned.check_metadata(&file).map_err(|_| {
        SandboxProbeFailure::Hard("Browser process executable inode or metadata does not match")
    })?;
    let link_after = fs::read_link(&exe_path)
        .map_err(|_| SandboxProbeFailure::Hard("Browser process exe link changed"))?;
    if link_after != link_before {
        return Err(SandboxProbeFailure::Hard(
            "Browser process executable changed during verification",
        ));
    }
    let named_after = open_absolute_file(expected_path).map_err(|_| {
        SandboxProbeFailure::Hard("Browser worker named executable cannot be reopened")
    })?;
    pinned.check_metadata(&named_after).map_err(|_| {
        SandboxProbeFailure::Hard(
            "Browser worker named executable metadata does not match pinned binary",
        )
    })?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn linux_verify_namespace_fd(
    fd: &BorrowedFd<'_>,
    expected_kind: KernelNamespaceKind,
) -> Result<KernelNamespaceId, SandboxProbeFailure> {
    let expected_type = match expected_kind {
        KernelNamespaceKind::Pid => libc::CLONE_NEWPID,
        KernelNamespaceKind::User => libc::CLONE_NEWUSER,
    };
    // SAFETY: `NS_GET_NSTYPE` inspects the namespace type of an open nsfs FD.
    let actual_type = unsafe { libc::ioctl(fd.as_raw_fd(), libc::NS_GET_NSTYPE as _) };
    if actual_type < 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EPERM) {
            return Err(SandboxProbeFailure::PermissionDenied);
        }
        return Err(SandboxProbeFailure::Hard(
            "Browser process namespace type cannot be queried",
        ));
    }
    if actual_type != expected_type {
        return Err(SandboxProbeFailure::Hard(
            "Browser process namespace type mismatch",
        ));
    }
    let borrowed_file = std::mem::ManuallyDrop::new(unsafe { File::from_raw_fd(fd.as_raw_fd()) });
    let meta = borrowed_file
        .metadata()
        .map_err(|_| SandboxProbeFailure::Hard("Browser process namespace stat failed"))?;
    Ok(KernelNamespaceId {
        dev: meta.dev(),
        ino: meta.ino(),
    })
}

#[cfg(target_os = "linux")]
pub(super) fn open_linux_namespace(
    proc_root: &Path,
    pid: u32,
    kind: KernelNamespaceKind,
) -> Result<VerifiedNamespace, SandboxProbeFailure> {
    let (leaf, prefix) = match kind {
        KernelNamespaceKind::Pid => ("pid", "pid:["),
        KernelNamespaceKind::User => ("user", "user:["),
    };
    let ns_path = proc_root.join(pid.to_string()).join("ns").join(leaf);
    let link_before = fs::read_link(&ns_path)
        .map_err(|_| SandboxProbeFailure::Hard("Browser process namespace link is unreadable"))?;
    let mut options = OpenOptions::new();
    // Magic procfs `/proc/<pid>/ns/*` symlinks cannot be opened with `O_NOFOLLOW`.
    options
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK);
    let file = options
        .open(&ns_path)
        .map_err(|_| SandboxProbeFailure::Hard("Browser process namespace cannot be opened"))?;
    let fd: OwnedFd = file.into();
    // SAFETY: `fd` stays valid across verification.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd.as_raw_fd()) };
    let id = linux_verify_namespace_fd(&borrowed, kind)?;
    let link_after = fs::read_link(&ns_path)
        .map_err(|_| SandboxProbeFailure::Hard("Browser process namespace link changed"))?;
    let expected_link = format!("{prefix}{}]", id.ino);
    if link_before != link_after || link_before.to_string_lossy() != expected_link {
        return Err(SandboxProbeFailure::Hard(
            "Browser process namespace link disagrees with descriptor",
        ));
    }
    Ok(VerifiedNamespace {
        id,
        fd: Some(Arc::new(fd)),
    })
}

#[cfg(target_os = "linux")]
fn linux_parent_namespace(
    ns: &VerifiedNamespace,
    expected_kind: KernelNamespaceKind,
) -> Result<Option<VerifiedNamespace>, SandboxProbeFailure> {
    let Some(fd) = ns.fd.as_ref() else {
        return Err(SandboxProbeFailure::Hard(
            "Browser process namespace descriptor is unavailable",
        ));
    };
    // SAFETY: `NS_GET_PARENT` takes no extra argument and returns a new owned FD on success.
    let raw = unsafe { libc::ioctl(fd.as_raw_fd(), libc::NS_GET_PARENT as _) };
    if raw < 0 {
        let err = io::Error::last_os_error();
        return match err.raw_os_error() {
            Some(libc::EPERM) => Err(SandboxProbeFailure::PermissionDenied),
            Some(libc::ENOENT) => Ok(None),
            _ => Err(SandboxProbeFailure::Hard(
                "Browser process parent namespace query failed",
            )),
        };
    }
    // SAFETY: kernel returned a newly allocated namespace file descriptor.
    let parent_fd = unsafe { OwnedFd::from_raw_fd(raw as RawFd) };
    // SAFETY: `parent_fd` stays alive while verifying type and metadata.
    let borrowed = unsafe { BorrowedFd::borrow_raw(parent_fd.as_raw_fd()) };
    let id = linux_verify_namespace_fd(&borrowed, expected_kind)?;
    Ok(Some(VerifiedNamespace {
        id,
        fd: Some(Arc::new(parent_fd)),
    }))
}

#[cfg(target_os = "linux")]
pub(super) fn linux_pid_ns_owner_user_ns(
    pid_ns: &VerifiedNamespace,
) -> Result<VerifiedNamespace, SandboxProbeFailure> {
    let Some(fd) = pid_ns.fd.as_ref() else {
        return Err(SandboxProbeFailure::Hard(
            "Browser PID namespace descriptor is unavailable",
        ));
    };
    // SAFETY: `NS_GET_USERNS` returns a new owned descriptor to the owning user namespace.
    let raw = unsafe { libc::ioctl(fd.as_raw_fd(), libc::NS_GET_USERNS as _) };
    if raw < 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EPERM) {
            return Err(SandboxProbeFailure::PermissionDenied);
        }
        return Err(SandboxProbeFailure::Hard(
            "Browser PID namespace owner query failed",
        ));
    }
    // SAFETY: kernel returned a newly allocated user namespace file descriptor.
    let owner_fd = unsafe { OwnedFd::from_raw_fd(raw as RawFd) };
    // SAFETY: `owner_fd` stays alive while verifying type and metadata.
    let borrowed = unsafe { BorrowedFd::borrow_raw(owner_fd.as_raw_fd()) };
    let id = linux_verify_namespace_fd(&borrowed, KernelNamespaceKind::User)?;
    Ok(VerifiedNamespace {
        id,
        fd: Some(Arc::new(owner_fd)),
    })
}
