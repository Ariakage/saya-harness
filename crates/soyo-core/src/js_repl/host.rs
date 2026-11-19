//! Host-side supervision of one `__soyo-js-worker` inside a prepared
//! environment. Owns the worker process group, the bounded stdio protocol and
//! the deadline/cancellation race. Permission decisions never happen here:
//! each bridged call is handed to the caller's gate one message at a time.

use super::{
    MAX_WIRE_BYTES, ReplEngine, ReplLimits, WORKER_COMMAND, WorkerInit, WorkerMessage, protocol,
};
/// Embedded CommonJS bootstrap evaluated by `node -e` inside the sandbox.
const NODE_BOOTSTRAP: &str = include_str!("node_bootstrap.js");
use crate::{
    boundary::Cancellation,
    contract::{BoundaryError, ErrorInfo, ToolResult, error_codes},
    execution::{BackendKind, PreparedEnvironment},
    tools::exec::{ExecConfig, ExecPlan, ManagedProcess, environment_digest},
};
use std::{collections::BTreeMap, path::Path, sync::Arc, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

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

/// One supervised worker. Dropping it kills the process group via
/// `ManagedProcess`; `shutdown` reaps the leader and releases the lease.
pub(crate) struct ReplWorker {
    process: Option<ManagedProcess>,
    stdin: tokio::process::ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
    stderr: Option<tokio::task::JoinHandle<()>>,
    stderr_buffer: Arc<std::sync::Mutex<Vec<u8>>>,
}
impl ReplWorker {
    /// Launch the worker inside `environment` under the run's own sandbox
    /// policy. No tool call is dispatched here; the process only receives the
    /// bounded init frame afterwards. The explicit-host compatibility backend
    /// is rejected: F-21 requires the native isolation selected by D-082/083.
    /// `engine` selects the embedded QuickJS worker (`B1`) or the controlled
    /// external Node runtime (`B2`); the Node program must already be exposed
    /// to the sandbox policy as a read-only file.
    pub(crate) fn spawn(
        environment: &PreparedEnvironment,
        workspace: &Path,
        timeout: Duration,
        engine: &ReplEngine,
        limits: &ReplLimits,
    ) -> Result<Self, BoundaryError> {
        if environment.identity().backend == BackendKind::Host {
            return Err(failure(
                "sandbox_capability_unavailable",
                "js_repl requires a native isolated backend, not the host compatibility mode",
            ));
        }
        let argv = match engine {
            ReplEngine::Quickjs => environment.internal_worker_argv(WORKER_COMMAND)?,
            ReplEngine::Node { program } => vec![
                program.as_os_str().to_owned(),
                format!(
                    "--max-old-space-size={}",
                    (limits.memory_bytes / (1024 * 1024)).max(1)
                )
                .into(),
                format!("--stack-size={}", (limits.stack_bytes / 1024).max(1)).into(),
                "-e".into(),
                NODE_BOOTSTRAP.into(),
            ],
        };
        let argv = argv
            .iter()
            .map(|value| {
                value
                    .to_str()
                    .map(str::to_owned)
                    .ok_or_else(|| failure("invalid_path", "worker argv must be UTF-8"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut env = BTreeMap::new();
        environment.apply_environment(&mut env)?;
        if matches!(engine, ReplEngine::Node { .. }) {
            // OpenSSL reads its global config at startup; an empty config at
            // /dev/null keeps prefix installs (openssl.cnf under etc/) from
            // needing any extra filesystem exposure.
            env.entry("OPENSSL_CONF".into())
                .or_insert_with(|| "/dev/null".into());
        }
        let plan = ExecPlan {
            argv,
            cwd: workspace.to_path_buf(),
            timeout,
            environment_digest: environment_digest(&env, Some(environment)),
        };
        let cwd = cap_std::fs::Dir::open_ambient_dir(workspace, cap_std::ambient_authority())
            .map_err(|error| failure("invalid_path", error.to_string()))?;
        let config = ExecConfig {
            default_timeout: timeout,
            max_timeout: timeout,
            stdout_max: MAX_WIRE_BYTES,
            stderr_max: 64 * 1024,
            execution: Some(environment.clone()),
            environment: crate::config::Environment::from_pairs([]),
            ..ExecConfig::default()
        };
        let mut process = ManagedProcess::spawn(&plan, cwd, env, &config, true)?;
        let stdin = process
            .take_stdin()
            .ok_or_else(|| failure("spawn_error", "worker stdin unavailable"))?;
        let stdout = process
            .take_stdout()
            .ok_or_else(|| failure("spawn_error", "worker stdout unavailable"))?;
        // Drain stderr into a shared bounded buffer so worker diagnostics
        // never block the pipe; the head of the buffer survives for failure
        // reporting while excess output is dropped.
        let stderr_buffer = Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let stderr = process.take_stderr().map(|stderr| {
            let buffer = Arc::clone(&stderr_buffer);
            tokio::spawn(async move {
                let mut bounded = stderr.take(64 * 1024);
                let mut chunk = [0u8; 4096];
                loop {
                    match bounded.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(read) => {
                            if let Ok(mut sink) = buffer.lock() {
                                let keep = (64 * 1024usize).saturating_sub(sink.len());
                                sink.extend_from_slice(&chunk[..read.min(keep)]);
                            }
                        }
                    }
                }
            })
        });
        Ok(Self {
            process: Some(process),
            stdin,
            stdout: BufReader::new(stdout),
            stderr,
            stderr_buffer,
        })
    }
    /// Bounded single-line stderr tail for failure diagnostics; engine
    /// crashes are otherwise invisible once the protocol channel goes quiet.
    pub(crate) fn stderr_tail(&self) -> Option<String> {
        let buffer = self.stderr_buffer.lock().ok()?;
        if buffer.is_empty() {
            return None;
        }
        let tail = if buffer.len() > 2048 {
            &buffer[buffer.len() - 2048..]
        } else {
            &buffer[..]
        };
        let text = String::from_utf8_lossy(tail);
        let line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        (!line.is_empty()).then_some(line)
    }
    /// Write the init frame carrying script and engine limits.
    pub(crate) async fn initialize(
        &mut self,
        init: &WorkerInit,
        cancel: &dyn Cancellation,
        deadline: Duration,
    ) -> Result<(), BoundaryError> {
        self.write(&protocol::encode(init)?, cancel, deadline).await
    }
    /// Read the next worker message within the deadline. `Ok(None)` means the
    /// worker exited (or closed its channel) before another message arrived.
    pub(crate) async fn next(
        &mut self,
        cancel: &dyn Cancellation,
        deadline: Duration,
    ) -> Result<Option<WorkerMessage>, BoundaryError> {
        let line = self.read_line(cancel, deadline).await?;
        match line {
            Some(line) => protocol::decode(&line).map(Some),
            None => Ok(None),
        }
    }
    /// Answer one pending bridged call with the dispatched result.
    pub(crate) async fn reply(
        &mut self,
        seq: u32,
        result: &ToolResult,
        cancel: &dyn Cancellation,
        deadline: Duration,
    ) -> Result<(), BoundaryError> {
        let bytes = protocol::encode(&protocol::HostMessage::Result {
            seq,
            result: result.clone(),
        })?;
        self.write(&bytes, cancel, deadline).await
    }
    /// Stop the worker and reap it. A still-running group is SIGKILLed; the
    /// environment lease is released once the child is reaped.
    pub(crate) async fn shutdown(mut self, cancel: &dyn Cancellation) -> Result<(), BoundaryError> {
        let mut process = self
            .process
            .take()
            .ok_or_else(|| failure("process_error", "worker already reaped"))?;
        if let Some(stderr) = self.stderr.take() {
            stderr.abort();
        }
        process
            .wait(cancel, Duration::from_secs(5))
            .await
            .map(|_| ())
    }
    async fn write(
        &mut self,
        bytes: &[u8],
        cancel: &dyn Cancellation,
        deadline: Duration,
    ) -> Result<(), BoundaryError> {
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(BoundaryError::new(error_codes::CANCELLED, "run cancelled")),
            () = tokio::time::sleep(deadline) => Err(failure(error_codes::TOOL_TIMEOUT, "js_repl worker timed out")),
            result = async {
                self.stdin.write_all(bytes).await?;
                self.stdin.flush().await
            } => {
                result.map_err(|error| failure("repl_worker_error", format!("worker channel write failed: {error}")))
            }
        }
    }
    async fn read_line(
        &mut self,
        cancel: &dyn Cancellation,
        deadline: Duration,
    ) -> Result<Option<Vec<u8>>, BoundaryError> {
        let mut line = Vec::new();
        let mut bounded = (&mut self.stdout).take((MAX_WIRE_BYTES as u64).saturating_add(1));
        let outcome: Result<usize, BoundaryError> = tokio::select! {
            biased;
            () = cancel.cancelled() => Err(BoundaryError::new(error_codes::CANCELLED, "run cancelled")),
            () = tokio::time::sleep(deadline) => Err(failure(error_codes::TOOL_TIMEOUT, "js_repl worker timed out")),
            result = bounded.read_until(b'\n', &mut line) => {
                result.map_err(|error| failure("repl_worker_error", format!("worker channel read failed: {error}")))
            }
        };
        match outcome? {
            0 => Ok(None),
            _ if line.len() > MAX_WIRE_BYTES => Err(failure(
                "worker_rpc_too_large",
                "worker message exceeds the wire limit",
            )),
            _ => Ok(Some(line)),
        }
    }
}
