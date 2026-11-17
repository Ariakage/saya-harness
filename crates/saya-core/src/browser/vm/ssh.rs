use super::*;

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
pub(super) fn vm_command_outcome(
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
pub(super) fn vm_command_result(
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

pub(super) fn strict_ssh_config(
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
pub(super) struct ReverseForward {
    pub(super) remote_socket: String,
    pub(super) local_port: u16,
}

pub(super) fn ssh_argv(
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

pub(super) fn write_private(path: &Path, bytes: &[u8]) -> Result<(), BoundaryError> {
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
pub(super) struct VmHostEnvironment {
    pub(super) environment: PreparedEnvironment,
    pub(super) base: PathBuf,
    pub(super) directories: BrowserSandboxDirectories,
}

impl VmHostEnvironment {
    pub(super) fn create(
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
pub(super) fn host_policy(
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
    pub(super) settings: BrowserVmSettings,
    pub(super) config: ExecConfig,
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

    pub(super) async fn run_once(
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
            boundary_to_browser_failure(vm_unavailable(
                "Browser VM host channel cannot be prepared",
            ))
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

pub(super) fn open_directory(path: &Path) -> Result<File, BrowserFailure> {
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

pub(super) const PROBE_SCRIPT: &str = r#"set -eu
LC_ALL=C
export LC_ALL
for f in "$@"; do
  stat -c '%s|%a|%F' -- "$f"
  sha256sum -- "$f"
done
"#;

pub(super) const CAT_SCRIPT: &str = "set -eu\ncat -- \"$1\"\n";

pub(super) const NODE_VERSION_SCRIPT: &str = "set -eu\n\"$1\" --version\n";
pub(super) const INSTALLATION_PROBE_SCRIPT: &str = "set -eu\ncd /\nexec env -i PATH=/usr/bin:/bin \"$1\" --no-addons - \"$2\" \"$3\" \"$4\" \"$5\"\n";
pub(super) const INSTALLATION_PROBE: &str = include_str!("../worker/installation_probe.js");

pub(super) const PREPARE_RUN_SCRIPT: &str = r#"set -eu
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

pub(super) const LAUNCH_SCRIPT: &str = r#"set -eu
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
pub(super) const PID_SCRIPT: &str = r#"set -eu
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

pub(super) const CLEANUP_SCRIPT: &str = r#"set -eu
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

pub(super) const SANDBOX_SCRIPT: &str = r#"set -eu
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
