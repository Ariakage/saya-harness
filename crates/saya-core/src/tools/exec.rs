//! Local process execution for controlled runtime dispatch. Process groups are
//! cleaned up on cancellation; this is not an OS sandbox.

use super::read::ReadTools;
use crate::{
    boundary::{Cancellation, Operation, Store, StoreChange, Tool, ToolContext, ToolDefinition},
    config::Environment,
    contract::{BoundaryError, ErrorInfo, PermissionMode, ToolCall, ToolResult, WorkMode},
};
use nix::{
    errno::Errno,
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    io,
    os::fd::AsRawFd,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    time::{sleep, timeout},
};

/// Fixed defaults and host-controlled environment policy. The model cannot
/// increase limits or select arbitrary inherited host variables.
#[derive(Debug, Clone)]
pub struct ExecConfig {
    pub default_timeout: Duration,
    pub max_timeout: Duration,
    pub stdout_max: usize,
    pub stderr_max: usize,
    pub inherit_extra: BTreeSet<String>,
    pub secret_env_names: BTreeSet<String>,
    /// Captured trusted environment used for inheritance, references and
    /// configured credential redaction. Values are omitted from Debug output.
    pub environment: Environment,
}
impl Default for ExecConfig {
    fn default() -> Self {
        Self {
            default_timeout: Duration::from_secs(120),
            max_timeout: Duration::from_secs(600),
            stdout_max: 64 * 1024,
            stderr_max: 64 * 1024,
            inherit_extra: BTreeSet::new(),
            secret_env_names: BTreeSet::new(),
            environment: Environment::capture(),
        }
    }
}
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "source", content = "value", rename_all = "snake_case")]
pub enum EnvValue {
    Literal(String),
    Reference(String),
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecArgs {
    pub argv: Vec<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    #[serde(default)]
    pub env: BTreeMap<String, EnvValue>,
}
/// A command normalized before authorization. `argv[0]` is an absolute path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecPlan {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub timeout: Duration,
    /// SHA-256 of the exact filtered environment; values are never displayed.
    pub environment_digest: String,
}
/// The runtime must provide an authorization decision bound to the operation.
pub trait ExecGate: Send + Sync {
    fn authorize(
        &self,
        plan: &ExecPlan,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<bool, BoundaryError>;
    /// Release any dispatch lease after completion, cancellation or error.
    fn finish(&self) -> Result<(), BoundaryError> {
        Ok(())
    }
}
struct ExecLease<'a>(&'a dyn ExecGate);
impl Drop for ExecLease<'_> {
    fn drop(&mut self) {
        let _ = self.0.finish();
    }
}
/// Result of a local command. stdout and stderr are decoded with UTF-8 loss
/// replacement; truncation flags describe retained bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecResult {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub timed_out: bool,
    pub cancelled: bool,
}
/// Command adapter; caller must supply policy and an execution-intent store.
pub struct ExecTool {
    files: Arc<ReadTools>,
    store: Arc<dyn Store>,
    gate: Arc<dyn ExecGate>,
    config: ExecConfig,
}
impl ExecTool {
    pub fn new(
        files: Arc<ReadTools>,
        store: Arc<dyn Store>,
        gate: Arc<dyn ExecGate>,
        config: ExecConfig,
    ) -> Self {
        Self {
            files,
            store,
            gate,
            config,
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
fn tool_failure(error: BoundaryError) -> ToolResult {
    ToolResult {
        ok: false,
        data: None,
        error: Some(error.0),
        truncated: false,
    }
}
fn is_base(name: &str) -> bool {
    matches!(name, "PATH" | "HOME" | "TMPDIR" | "LANG") || name.starts_with("LC_")
}
fn valid_env_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['=', '\0'])
}
fn cwd_error(error: io::Error) -> BoundaryError {
    if matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory | io::ErrorKind::InvalidFilename
    ) || error.raw_os_error() == Some(libc::ELOOP)
    {
        failure("invalid_path", "command cwd must be an existing directory")
    } else {
        failure("io_error", error.to_string())
    }
}

impl ExecTool {
    /// Resolve command identity without spawning or authorizing it. This lets
    /// the policy and one-shot approval bind the exact command and environment.
    pub fn plan(&self, call: &ToolCall, context: &ToolContext) -> Result<ExecPlan, BoundaryError> {
        self.plan_with_environment_keys(call, context)
            .map(|(plan, _)| plan)
    }
    /// Prepare the command and its actual filtered environment names together.
    pub fn plan_with_environment_keys(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<(ExecPlan, Vec<String>), BoundaryError> {
        if call.name != "exec" {
            return Err(failure("invalid_args", "tool name does not match adapter"));
        }
        if std::fs::canonicalize(&context.workspace)
            .map_err(|e| failure("invalid_workspace", e.to_string()))?
            != self.files.workspace
        {
            return Err(failure(
                "invalid_workspace",
                "tool workspace differs from run",
            ));
        }
        if self.files.bypass && context.run.permissions != PermissionMode::Bypass {
            return Err(failure(
                "path_denied",
                "unrestricted roots require bypass permissions",
            ));
        }
        let args: ExecArgs = serde_json::from_str(&call.arguments)
            .map_err(|e| failure("invalid_args", e.to_string()))?;
        let (plan, _, environment) = self.prepare(&args, context)?;
        let keys = environment
            .keys()
            .map(|key| key.to_string_lossy().into_owned())
            .collect();
        Ok((plan, keys))
    }
    fn prepare(
        &self,
        args: &ExecArgs,
        context: &ToolContext,
    ) -> Result<(ExecPlan, cap_std::fs::Dir, BTreeMap<OsString, OsString>), BoundaryError> {
        if args.argv.is_empty()
            || args.argv[0].is_empty()
            || args.argv.iter().any(|a| a.contains('\0'))
        {
            return Err(failure(
                "invalid_args",
                "argv must contain a nonempty executable and no NUL",
            ));
        }
        if self.config.max_timeout.is_zero()
            || self.config.default_timeout.is_zero()
            || self.config.default_timeout > self.config.max_timeout
        {
            return Err(failure(
                "invalid_config",
                "invalid command timeout configuration",
            ));
        }
        let command_timeout = args
            .timeout_seconds
            .map(Duration::from_secs)
            .unwrap_or(self.config.default_timeout);
        if command_timeout.is_zero() || command_timeout > self.config.max_timeout {
            return Err(failure(
                "invalid_args",
                "command timeout exceeds configured limit",
            ));
        }
        let requested_cwd = args.cwd.as_deref().unwrap_or(&context.workspace);
        if requested_cwd.as_os_str().as_bytes().contains(&0) {
            return Err(failure("invalid_args", "command cwd must not contain NUL"));
        }
        let requested_cwd =
            std::fs::canonicalize(context.workspace.join(requested_cwd)).map_err(cwd_error)?;
        let (cwd, root, relative) = self
            .files
            .resolve(&requested_cwd)
            .map_err(|e| failure(e.code, e.message))?;
        let cwd_dir = root.dir.open_dir(relative).map_err(cwd_error)?;
        let mut env = BTreeMap::new();
        for (key, value) in self.config.environment.iter() {
            if let Some(name) = key.to_str()
                && !self.config.secret_env_names.contains(name)
                && (is_base(name) || self.config.inherit_extra.contains(name))
            {
                env.insert(key.clone(), value.clone());
            }
        }
        for (key, value) in &args.env {
            if !valid_env_name(key) || self.config.secret_env_names.contains(key) {
                return Err(failure(
                    "invalid_args",
                    "invalid or forbidden environment key",
                ));
            }
            let value = match value {
                EnvValue::Literal(value) if !value.contains('\0') => OsString::from(value),
                EnvValue::Reference(name)
                    if valid_env_name(name)
                        && !self.config.secret_env_names.contains(name)
                        && (is_base(name) || self.config.inherit_extra.contains(name)) =>
                {
                    self.config
                        .environment
                        .get_os(name)
                        .map(OsStr::to_owned)
                        .ok_or_else(|| {
                            failure(
                                "invalid_args",
                                "referenced environment variable is unavailable",
                            )
                        })?
                }
                _ => {
                    return Err(failure(
                        "invalid_args",
                        "invalid or unauthorized environment value",
                    ));
                }
            };
            env.insert(key.into(), value);
        }
        let executable = resolve_executable(&args.argv[0], &cwd, env.get(OsStr::new("PATH")))?;
        let mut argv = args.argv.clone();
        argv[0] = executable
            .to_str()
            .ok_or_else(|| failure("invalid_path", "executable path is not UTF-8"))?
            .to_owned();
        let mut digest = sha2::Sha256::new();
        use sha2::Digest;
        for (key, value) in &env {
            let key = key.as_bytes();
            let value = value.as_bytes();
            digest.update((key.len() as u64).to_be_bytes());
            digest.update(key);
            digest.update((value.len() as u64).to_be_bytes());
            digest.update(value);
        }
        let environment_digest: String = digest
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Ok((
            ExecPlan {
                argv,
                cwd,
                timeout: command_timeout,
                environment_digest,
            },
            cwd_dir,
            env,
        ))
    }
}
pub(crate) fn resolve_executable(
    program: &str,
    cwd: &Path,
    path: Option<&OsString>,
) -> Result<PathBuf, BoundaryError> {
    let candidate = Path::new(program);
    let search: Vec<PathBuf> = if program.contains('/') || candidate.is_absolute() {
        vec![if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            cwd.join(candidate)
        }]
    } else {
        std::env::split_paths(
            path.ok_or_else(|| failure("invalid_config", "PATH is not available"))?,
        )
        .map(|dir| {
            if dir.is_absolute() {
                dir.join(program)
            } else {
                cwd.join(dir).join(program)
            }
        })
        .collect()
    };
    for candidate in search {
        if let Ok(real) = std::fs::canonicalize(candidate)
            && let Ok(meta) = std::fs::metadata(&real)
            && meta.is_file()
            && meta.permissions().mode() & 0o111 != 0
        {
            return Ok(real);
        }
    }
    Err(failure(
        "executable_not_found",
        "executable not found or not executable",
    ))
}

#[derive(Default)]
struct Output {
    data: Vec<u8>,
    truncated: bool,
}
async fn drain<R: AsyncRead + Unpin>(
    mut reader: R,
    max: usize,
    target: Arc<Mutex<Output>>,
) -> io::Result<()> {
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        let mut out = target
            .lock()
            .map_err(|_| io::Error::other("output lock poisoned"))?;
        let take = n.min(max.saturating_sub(out.data.len()));
        out.data.extend_from_slice(&buf[..take]);
        if take < n {
            out.truncated = true;
        }
    }
    Ok(())
}
fn signal_group(pid: Pid, signal: Signal) -> Result<(), BoundaryError> {
    match killpg(pid, signal) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(e) => Err(failure("process_cleanup_error", e.to_string())),
    }
}
fn child_group_pid(pid: u32) -> Result<Pid, BoundaryError> {
    let pid = i32::try_from(pid)
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or_else(|| {
            failure(
                "spawn_error",
                "child PID is outside the supported positive range",
            )
        })?;
    Ok(Pid::from_raw(pid))
}
struct ProcessGroup(Option<Pid>);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if let Some(pid) = self.0
            && leader_exited(pid).is_ok()
        {
            let _ = signal_group(pid, Signal::SIGKILL);
        }
    }
}

// Observe the leader without reaping it. Keeping its PID reserved until after
// signaling the group prevents signaling an unrelated reused process group.
fn leader_exited(pid: Pid) -> Result<bool, BoundaryError> {
    // SAFETY: siginfo is initialized, points to writable storage, and the
    // PID identifies our child. WNOHANG keeps this call nonblocking;
    // WNOWAIT leaves the child for tokio's Child::wait to reap.
    unsafe {
        let mut info: libc::siginfo_t = std::mem::zeroed();
        if libc::waitid(
            libc::P_PID,
            pid.as_raw() as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        ) == -1
        {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                Ok(false)
            } else {
                Err(failure("process_error", error.to_string()))
            }
        } else {
            Ok(info.si_pid() != 0)
        }
    }
}
async fn wait_leader(pid: Pid) -> Result<(), BoundaryError> {
    loop {
        if leader_exited(pid)? {
            return Ok(());
        }
        sleep(Duration::from_millis(10)).await;
    }
}

// On macOS, a group containing only zombies can return EPERM. If no signal
// could be sent, verify its disappearance after reaping; a group that remains
// inaccessible is a cleanup failure, not an assumed success.
async fn confirm_group_gone(pid: Pid) -> Result<(), BoundaryError> {
    let check = async {
        loop {
            match killpg(pid, None) {
                Err(Errno::ESRCH) => return Ok(()),
                Ok(()) | Err(Errno::EPERM) => sleep(Duration::from_millis(10)).await,
                Err(e) => return Err(failure("process_cleanup_error", e.to_string())),
            }
        }
    };
    timeout(Duration::from_secs(1), check).await.map_err(|_| {
        failure(
            "process_cleanup_error",
            "could not confirm process group cleanup",
        )
    })?
}

async fn run_process(
    plan: &ExecPlan,
    cwd: cap_std::fs::Dir,
    env: BTreeMap<OsString, OsString>,
    config: &ExecConfig,
    cancel: &dyn Cancellation,
) -> Result<ExecResult, BoundaryError> {
    if cancel.is_cancelled() {
        return Err(failure("cancelled", "command cancelled before spawn"));
    }
    let mut command = Command::new(&plan.argv[0]);
    command
        .args(&plan.argv[1..])
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    let cwd_file = cwd.into_std_file();
    // SAFETY: after fork this callback only invokes async-signal-safe fchdir on
    // an open directory fd retained in the closure; it performs no allocation.
    unsafe {
        command.pre_exec(move || {
            if libc::fchdir(cwd_file.as_raw_fd()) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|e| failure("spawn_error", e.to_string()))?;
    let pid = child
        .id()
        .ok_or_else(|| failure("spawn_error", "child PID unavailable"))?;
    let pid = child_group_pid(pid)?;
    let mut group = ProcessGroup(Some(pid));
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| failure("spawn_error", "stdout pipe unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| failure("spawn_error", "stderr pipe unavailable"))?;
    let out = Arc::new(Mutex::new(Output::default()));
    let err = Arc::new(Mutex::new(Output::default()));
    let mut out_task = tokio::spawn(drain(stdout, config.stdout_max, out.clone()));
    let mut err_task = tokio::spawn(drain(stderr, config.stderr_max, err.clone()));
    let mut timed_out = false;
    let mut cancelled = false;
    let exited = tokio::select! {
        biased;
        _ = cancel.cancelled() => { cancelled = true; false },
        _ = sleep(plan.timeout) => { timed_out = true; false },
        result = wait_leader(pid) => { result?; true },
    };
    if !exited {
        match killpg(pid, Signal::SIGTERM) {
            Ok(()) | Err(Errno::ESRCH) | Err(Errno::EPERM) => {}
            Err(e) => return Err(failure("process_cleanup_error", e.to_string())),
        }
        match timeout(Duration::from_secs(5), wait_leader(pid)).await {
            Ok(result) => result?,
            Err(_) => {
                match killpg(pid, Signal::SIGKILL) {
                    Ok(()) | Err(Errno::ESRCH) | Err(Errno::EPERM) => {}
                    Err(e) => return Err(failure("process_cleanup_error", e.to_string())),
                }
                timeout(Duration::from_secs(5), wait_leader(pid))
                    .await
                    .map_err(|_| {
                        failure("process_cleanup_error", "process did not exit after kill")
                    })??;
            }
        }
    }
    // Signal while the unreaped leader still reserves this process-group ID.
    let needs_confirmation = match killpg(pid, Signal::SIGKILL) {
        Ok(()) | Err(Errno::ESRCH) => false,
        Err(Errno::EPERM) => true,
        Err(e) => return Err(failure("process_cleanup_error", e.to_string())),
    };
    group.0 = None;
    let status = child
        .wait()
        .await
        .map_err(|e| failure("process_error", e.to_string()))?;
    if needs_confirmation {
        confirm_group_gone(pid).await?;
    }
    for task in [&mut out_task, &mut err_task] {
        match timeout(Duration::from_secs(5), &mut *task).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(e))) => return Err(failure("process_io_error", e.to_string())),
            Ok(Err(e)) => return Err(failure("process_io_error", e.to_string())),
            Err(_) => {
                task.abort();
                return Err(failure(
                    "process_io_error",
                    "output pipe did not close after cleanup",
                ));
            }
        }
    }
    let out = out
        .lock()
        .map_err(|_| failure("process_io_error", "stdout lock poisoned"))?;
    let err = err
        .lock()
        .map_err(|_| failure("process_io_error", "stderr lock poisoned"))?;
    Ok(ExecResult {
        exit_code: status.code(),
        stdout: String::from_utf8_lossy(&out.data).into_owned(),
        stderr: String::from_utf8_lossy(&err.data).into_owned(),
        stdout_truncated: out.truncated,
        stderr_truncated: err.truncated,
        timed_out,
        cancelled,
    })
}

fn bound_output(value: &mut String, limit: usize, truncated: &mut bool) {
    if value.len() > limit {
        let mut end = limit;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
        *truncated = true;
    }
}

impl Tool for ExecTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "exec".into(),
            description: "Run a directly specified local command".into(),
            input_schema: serde_json::json!({"type":"object","properties":{
                "argv":{"type":"array","minItems":1,"items":{"type":"string"}},
                "cwd":{"type":["string","null"]},"timeout_seconds":{"type":["integer","null"],"minimum":1},
                "env":{"type":"object","additionalProperties":{"type":"object","properties":{"source":{"enum":["literal","reference"]},"value":{"type":"string"}},"required":["source","value"],"additionalProperties":false}}
            },"required":["argv"],"additionalProperties":false}),
        }
    }
    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        context: &'a ToolContext,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, ToolResult> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(failure("cancelled", "command cancelled"));
            }
            if context.run.work_mode == WorkMode::Plan {
                return Ok(tool_failure(failure(
                    "mode_denied",
                    "PLAN does not allow command execution",
                )));
            }
            if std::fs::canonicalize(&context.workspace)
                .map_err(|e| failure("invalid_workspace", e.to_string()))?
                != self.files.workspace
            {
                return Ok(tool_failure(failure(
                    "invalid_workspace",
                    "tool workspace differs from run",
                )));
            }
            if self.files.bypass && context.run.permissions != PermissionMode::Bypass {
                return Ok(tool_failure(failure(
                    "path_denied",
                    "unrestricted roots require bypass permissions",
                )));
            }
            if call.name != "exec" {
                return Ok(tool_failure(failure(
                    "invalid_args",
                    "tool name does not match adapter",
                )));
            }
            let args: ExecArgs = match serde_json::from_str(&call.arguments) {
                Ok(args) => args,
                Err(e) => return Ok(tool_failure(failure("invalid_args", e.to_string()))),
            };
            let (plan, _, env) = match self.prepare(&args, context) {
                Ok(value) => value,
                Err(error) => return Ok(tool_failure(error)),
            };
            if !self.gate.authorize(&plan, call, context)? {
                return Ok(tool_failure(failure(
                    "permission_denied",
                    "command not authorized",
                )));
            }
            let _lease = ExecLease(self.gate.as_ref());
            // Explicitly supplied arguments can contain configured credential
            // values; redact known values before persisting the intent.
            let secrets: Vec<String> = self
                .config
                .secret_env_names
                .iter()
                .filter_map(|name| {
                    self.config
                        .environment
                        .get_os(name)?
                        .to_str()
                        .map(str::to_owned)
                })
                .filter(|value| !value.is_empty())
                .collect();
            let mut saved_call = call.clone();
            let mut saved_args: serde_json::Value = serde_json::from_str(&call.arguments)
                .map_err(|e| failure("invalid_args", e.to_string()))?;
            crate::redaction::redact(&mut saved_args, &secrets)?;
            saved_call.arguments = serde_json::to_string(&saved_args)
                .map_err(|e| failure("serialization_error", e.to_string()))?;
            self.store
                .commit(&[StoreChange::ToolIntent {
                    run_id: context.run.id,
                    call: saved_call,
                }])
                .await?;
            let post_intent = async {
                if cancel.is_cancelled() {
                    return Err(failure("cancelled", "command cancelled before spawn"));
                }
                let (current, cwd, current_env) = self.prepare(&args, context)?;
                if current != plan
                    || current_env != env
                    || !self.gate.authorize(&current, call, context)?
                {
                    return Err(failure(
                        "permission_denied",
                        "command authorization no longer valid",
                    ));
                }
                run_process(&current, cwd, current_env, &self.config, cancel).await
            }
            .await;
            let process_result = match post_intent {
                Ok(value) => value,
                Err(error) => {
                    // Once spawned, an error can leave side effects unknown. Do
                    // not claim that the operation did not execute or retry it.
                    if matches!(
                        error.0.code.as_str(),
                        "process_error" | "process_io_error" | "process_cleanup_error"
                    ) {
                        self.store
                            .commit(&[StoreChange::ToolState {
                                run_id: context.run.id,
                                call_id: call.call_id.clone(),
                                state: crate::contract::ToolState::Unknown,
                            }])
                            .await?;
                        return Err(error);
                    }
                    let result = tool_failure(error.clone());
                    let result = super::write::save_result_with_secrets(
                        self.store.as_ref(),
                        call,
                        context,
                        &result,
                        &secrets,
                    )
                    .await?;
                    if error.0.code == "cancelled" {
                        return Err(error);
                    }
                    return Ok(result);
                }
            };
            let ok = process_result.exit_code == Some(0)
                && !process_result.timed_out
                && !process_result.cancelled;
            let error = if process_result.timed_out {
                Some(ErrorInfo {
                    code: "timeout".into(),
                    message: "command timed out".into(),
                })
            } else if process_result.cancelled {
                Some(ErrorInfo {
                    code: "cancelled".into(),
                    message: "command cancelled".into(),
                })
            } else if !ok {
                Some(ErrorInfo {
                    code: "command_failed".into(),
                    message: "command exited unsuccessfully".into(),
                })
            } else {
                None
            };
            let truncated = process_result.stdout_truncated || process_result.stderr_truncated;
            let data = serde_json::to_value(process_result)
                .map_err(|e| failure("serialization_error", e.to_string()))?;
            let result = ToolResult {
                ok,
                data: Some(data),
                error,
                truncated,
            };
            let mut result = super::write::sanitize_result(&result, &secrets)?;
            let mut process_result: ExecResult =
                serde_json::from_value(result.data.clone().unwrap_or(serde_json::Value::Null))
                    .map_err(|_| {
                        let mut error = failure(
                            "sensitive_data",
                            "command result schema could not be safely redacted",
                        );
                        error.2 = Some(Box::new(result.clone()));
                        error
                    })?;
            bound_output(
                &mut process_result.stdout,
                self.config.stdout_max,
                &mut process_result.stdout_truncated,
            );
            bound_output(
                &mut process_result.stderr,
                self.config.stderr_max,
                &mut process_result.stderr_truncated,
            );
            result.truncated = process_result.stdout_truncated || process_result.stderr_truncated;
            result.data = Some(serde_json::to_value(process_result).map_err(|_| {
                failure("serialization_error", "command result could not be encoded")
            })?);
            super::write::save_result(self.store.as_ref(), call, context, &result).await?;
            Ok(result)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn child_group_ids_require_a_positive_representable_pid() -> Result<(), BoundaryError> {
        for (raw, expected) in [(1, 1), (42, 42), (i32::MAX.unsigned_abs(), i32::MAX)] {
            assert_eq!(child_group_pid(raw)?.as_raw(), expected);
        }
        for raw in [0, i32::MAX.unsigned_abs() + 1, u32::MAX] {
            assert_eq!(
                child_group_pid(raw).expect_err("invalid child PID").0.code,
                "spawn_error"
            );
        }
        Ok(())
    }
    #[test]
    fn json_redaction_handles_escaped_sensitive_values() -> Result<(), BoundaryError> {
        let secret = "quote\"and\\slash".to_owned();
        let mut value = serde_json::json!({"argv":["program", format!("prefix-{secret}-suffix")]});
        crate::redaction::redact(&mut value, std::slice::from_ref(&secret))?;
        assert_eq!(value["argv"][1], "prefix-[REDACTED]-suffix");
        assert!(!value.to_string().contains("and\\\\slash"));
        Ok(())
    }
}
