use super::*;

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

pub(super) const UPLOAD_SCRIPT: &str = r#"set -eu
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

pub(super) struct RunnerRemoteControl {
    pub(super) settings: BrowserVmSettings,
    pub(super) runner: Arc<dyn VmCommandRunner>,
    pub(super) run_root: String,
    pub(super) pid_file: String,
    pub(super) cli_js: String,
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
pub(super) struct NeverCancel;
impl Cancellation for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}

struct VmSpawned {
    pub(super) process: ManagedProcess,
    pub(super) stdin: tokio::process::ChildStdin,
    pub(super) stdout: tokio::process::ChildStdout,
    pub(super) stderr_tail: Arc<Mutex<VecDeque<u8>>>,
    pub(super) stderr_task: Option<tokio::task::JoinHandle<()>>,
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

pub(super) fn classify_launch_failure(cancel: &dyn Cancellation, stderr: &[u8]) -> BrowserFailure {
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
pub(super) struct ConnectionCleanup {
    pub(super) control: Arc<dyn VmRemoteControl>,
    pub(super) done: std::sync::atomic::AtomicBool,
    pub(super) lock: AsyncMutex<()>,
}

impl ConnectionCleanup {
    pub(super) fn new(control: Arc<dyn VmRemoteControl>) -> Self {
        Self {
            control,
            done: std::sync::atomic::AtomicBool::new(false),
            lock: AsyncMutex::new(()),
        }
    }

    pub(super) async fn terminate(&self, expect_worker: bool) -> Result<(), BrowserFailure> {
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

    pub(super) fn pending(&self) -> bool {
        !self.done.load(Ordering::Acquire)
    }
}

/// Schedule the best-effort Drop cleanup. A Tokio runtime may be absent when
/// the provider is dropped during host shutdown; the remote outcome is then
/// explicitly logged as unknown rather than treated as success.
pub(super) fn schedule_drop_cleanup(cleanup: &Arc<ConnectionCleanup>) -> bool {
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
    pub(super) client: AsyncMutex<Option<crate::mcp::McpClient>>,
    pub(super) process: Arc<AsyncMutex<Option<ManagedProcess>>>,
    pub(super) protocol: String,
    pub(super) generation: AtomicU64,
    pub(super) pid: u32,
    pub(super) stderr_tail: Arc<Mutex<VecDeque<u8>>>,
    pub(super) cleanup: Arc<ConnectionCleanup>,
    pub(super) monitor: AsyncMutex<Option<tokio::task::JoinHandle<()>>>,
    /// Per-worker broker. Dropping it signals shutdown even when an outer
    /// caller never reaches `WorkerConnection::shutdown`.
    pub(super) network: Option<Arc<BrowserNetwork>>,
    /// Host Seatbelt environment that carries the exact broker port rule.
    pub(super) host: Option<Arc<VmHostEnvironment>>,
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

    async fn capture_screenshot(&self, cancel: &dyn Cancellation) -> Result<Value, BrowserFailure> {
        let client = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(BrowserFailure::new("cancelled", "Browser screenshot capture cancelled")),
            client = self.client.lock() => client,
        };
        let client = client
            .as_ref()
            .ok_or_else(|| BrowserFailure::new("worker_closed", "Browser VM worker is closed"))?;
        client
            .browser_capture_screenshot(cancel)
            .await
            .map_err(worker::capture_screenshot_failure)
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
pub(super) fn worker_launch_arguments(
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
    pub(super) settings: BrowserNetworkSettings,
    pub(super) log: Arc<BrowserNetworkLog>,
    pub(super) budget: Arc<RunNetworkBudget>,
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

    pub(super) async fn start(
        &self,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserNetwork, BoundaryError> {
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
    pub(super) settings: BrowserVmSettings,
    pub(super) runner: Arc<dyn VmCommandRunner>,
    pub(super) config: ExecConfig,
    /// Per-Run broker factory. Every worker lifecycle starts a fresh listener
    /// that shares one bounded observation log with the provider.
    pub(super) network: Arc<BrowserNetworkFactory>,
    /// Remote cleanup handles that could not be confirmed yet. They remain
    /// registered even when no connection was established, so a later launch
    /// or shutdown can retry instead of dropping the Run root silently. The
    /// entry is registered before the first await of a launch (and before a
    /// cleanup await), so dropping the launch future cannot lose the only
    /// handle to the remote directory.
    pub(super) pending_cleanup: Mutex<VecDeque<Arc<PendingCleanup>>>,
}

pub(super) struct PendingCleanup {
    pub(super) control: Arc<dyn VmRemoteControl>,
    /// Observational only: whether a worker may have been started for this Run
    /// root. It selects the cleanup path but never blocks a safe cleanup.
    pub(super) expect_worker: AtomicBool,
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
    pub(super) fn pending_entries(
        &self,
    ) -> std::sync::MutexGuard<'_, VecDeque<Arc<PendingCleanup>>> {
        match self.pending_cleanup.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Register a Run root before the first remote await. Registering an
    /// already-known handle updates its observation instead of duplicating it.
    pub(super) fn register_pending_cleanup(
        &self,
        control: Arc<dyn VmRemoteControl>,
        expect_worker: bool,
    ) {
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

    pub(super) fn update_pending_expect(
        &self,
        control: &Arc<dyn VmRemoteControl>,
        expect_worker: bool,
    ) {
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

    pub(super) fn remove_pending_cleanup(&self, control: &Arc<dyn VmRemoteControl>) {
        self.pending_entries()
            .retain(|entry| !Arc::ptr_eq(&entry.control, control));
    }

    pub(super) fn remote_root(&self) -> Result<String, BoundaryError> {
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

    pub(super) async fn cleanup(
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
    pub(super) async fn cleanup_or_retain(
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
    fn validated_launch_inputs(
        &self,
        request: &WorkerLaunchRequest,
    ) -> Result<(VmLocalPaths, String, String, String, String), BrowserFailure> {
        let paths = self.settings.preflight_local().map_err(|_| {
            boundary_to_browser_failure(vm_unavailable("Browser VM local inputs are unavailable"))
        })?;
        let run_root = self.remote_root().map_err(|_| {
            boundary_to_browser_failure(vm_unavailable("Browser VM remote temp root is invalid"))
        })?;
        let Some(identity) = request.identity.as_ref() else {
            return Err(remote_failure(
                "Browser VM launch has no verified worker identity",
            ));
        };
        if self.settings.identity_sha256.as_deref() != Some(identity.manifest_sha256.as_str()) {
            return Err(identity_failure(
                "Browser VM verified identity does not match the host input",
            ));
        }
        let node = identity
            .node
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| identity_failure("Browser VM verified node path is not UTF-8"))?;
        let chromium = identity
            .chromium
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| identity_failure("Browser VM verified Chromium path is not UTF-8"))?;
        let cli_js =
            identity.cli_js.to_str().map(str::to_owned).ok_or_else(|| {
                identity_failure("Browser VM verified worker CLI path is not UTF-8")
            })?;
        Ok((paths, run_root, node, chromium, cli_js))
    }

    async fn prepare_remote_run_root(
        &self,
        control: &Arc<dyn VmRemoteControl>,
        run_root: &str,
        vm_proxy_url: &str,
        cancel: &dyn Cancellation,
    ) -> Result<(), BrowserFailure> {
        let config = worker_config_json(&self.settings, run_root, vm_proxy_url).map_err(|_| {
            boundary_to_browser_failure(vm_unavailable("Browser VM worker config is invalid"))
        })?;
        let prepare_args = [run_root.to_owned()];
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
                    && String::from_utf8_lossy(&output.stdout).trim() == run_root =>
            {
                Ok(())
            }
            Ok(_) => {
                let failure = remote_failure("Browser VM Run root could not be prepared");
                Err(self
                    .cleanup_or_retain(control.clone(), false, failure)
                    .await)
            }
            Err(failure) if failure.code == "cancelled" => Err(failure),
            Err(failure) => Err(self
                .cleanup_or_retain(control.clone(), false, failure)
                .await),
        }
    }

    fn build_ssh_launch_argv(
        &self,
        paths: &VmLocalPaths,
        host: &VmHostEnvironment,
        launch_args: &[String],
        proxy_socket: &str,
        local_port: u16,
    ) -> Result<Vec<String>, BrowserFailure> {
        // The host channel's generated SSH config is written by the runner for
        // short commands; the long-lived launch needs its own copy.
        let ssh_config = strict_ssh_config(&self.settings, paths).map_err(|_| {
            boundary_to_browser_failure(vm_unavailable("Browser VM SSH config is invalid"))
        })?;
        let ssh_config_path = host.directories.config.join("ssh_config");
        write_private(&ssh_config_path, ssh_config.as_bytes())
            .map_err(|_| remote_failure("Browser VM SSH config could not be written"))?;
        ssh_argv(
            &self.settings,
            paths,
            &ssh_config_path,
            LAUNCH_SCRIPT,
            launch_args,
            Some(&ReverseForward {
                remote_socket: proxy_socket.to_owned(),
                local_port,
            }),
        )
        .map_err(|_| {
            boundary_to_browser_failure(vm_unavailable("Browser VM SSH launch input is invalid"))
        })
    }

    async fn query_remote_worker_pid(
        &self,
        pid_file: &str,
        node: &str,
        cli_js: &str,
        cancel: &dyn Cancellation,
    ) -> Option<u32> {
        let pid_args = [pid_file.to_owned(), node.to_owned(), cli_js.to_owned()];
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
        match pid_output {
            Ok(ref output) if output.succeeded() => parse_pid(&output.stdout),
            _ => None,
        }
    }

    pub(super) async fn launch_with_network(
        &self,
        request: WorkerLaunchRequest,
        cancel: &dyn Cancellation,
        network: Arc<BrowserNetwork>,
    ) -> Result<Arc<dyn WorkerConnection>, BrowserFailure> {
        let (paths, run_root, node, chromium, cli_js) = self.validated_launch_inputs(&request)?;
        // The SSH process may reach only the VM SSH port and this Run's broker
        // port. The host environment is created after the broker so the
        // loopback port is exact rather than a range.
        let host = Arc::new(
            VmHostEnvironment::create(&self.settings, Some(network.endpoint_port())).map_err(
                |_| {
                    boundary_to_browser_failure(vm_unavailable(
                        "Browser VM host channel cannot be prepared",
                    ))
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

        let vm_proxy_url = format!(
            "http://127.0.0.1:{}",
            crate::browser::network::VM_PROXY_PORT
        );
        self.prepare_remote_run_root(&control, &run_root, &vm_proxy_url, cancel)
            .await?;
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
            crate::browser::network::VM_PROXY_PORT,
            &pid_file,
            &node,
            &cli_js,
            &config_path,
            &chromium,
        );
        let argv = match self.build_ssh_launch_argv(
            &paths,
            &host,
            &launch_args,
            &proxy_socket,
            network.endpoint_port(),
        ) {
            Ok(argv) => argv,
            Err(failure) => {
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
        let Some(remote_pid) = self
            .query_remote_worker_pid(&pid_file, &node, &cli_js, cancel)
            .await
        else {
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
            return Err(boundary_to_browser_failure(vm_unavailable(
                "Browser VM requires a prepared host channel",
            )));
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

pub(super) fn parse_pid(output: &[u8]) -> Option<u32> {
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
    pub(super) settings: BrowserVmSettings,
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
    pub(super) host: VmHostEnvironment,
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
) -> Result<crate::browser::worker::PlaywrightMcpProvider, BoundaryError> {
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
    Ok(
        crate::browser::worker::PlaywrightMcpProvider::with_worker_backend(
            "vm-colima",
            verifier,
            planner,
            launcher,
            factory,
            inspector,
        )?
        .with_network_log(observation_log),
    )
}

pub(super) fn boundary_to_browser_failure(error: BoundaryError) -> BrowserFailure {
    BrowserFailure::new(error.0.code, error.0.message)
}
