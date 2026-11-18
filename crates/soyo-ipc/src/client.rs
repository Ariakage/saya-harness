//! Client for the `soyo helper` daemon: connect (auto-spawning if needed),
//! multiplexed request/response, streaming subscriptions, and shutdown.

use serde::Serialize;
use serde_json::Value;
use std::{
    collections::HashMap,
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::UnixStream,
    sync::{Mutex, mpsc, oneshot},
};

use crate::{
    framing::{FrameWriter, read_server_frame, split, write_frame},
    protocol::{ClientFrame, PROTOCOL_VERSION, ServerFrame},
    socket::{HelperTarget, verify_peer_euid},
    types::{DesktopError, DesktopStreamEnvelope, HelperInfoDto},
};

type PendingResult = oneshot::Sender<Result<Value, DesktopError>>;

enum PendingEntry {
    OneShot(PendingResult),
    Stream {
        result: PendingResult,
        events: mpsc::UnboundedSender<DesktopStreamEnvelope>,
    },
}

#[derive(Default)]
struct PendingMap {
    entries: HashMap<u64, PendingEntry>,
}

/// Result of [`HelperClient::helper_compat`]: whether the running helper is
/// incompatible relative to this client's version/protocol, whether runs or
/// other active clients keep it busy, and a short summary for prompts/logs.
pub struct HelperCompat {
    pub mismatch: bool,
    pub run_active: bool,
    pub busy: bool,
    pub detail: String,
}

/// Shared client handle. Clone-free use via `Arc` inside managed state.
pub struct HelperClient {
    writer: Mutex<FrameWriter>,
    next_id: AtomicU64,
    pending: Mutex<PendingMap>,
    closed: AtomicBool,
    target: Option<HelperTarget>,
}

impl HelperClient {
    /// Connect to the default environment's helper, spawning `soyo helper` if needed.
    pub async fn connect_or_spawn() -> Result<Arc<Self>, DesktopError> {
        let target = HelperTarget::from_environment()
            .map_err(|err| DesktopError::new("invalid_config", err.message().to_owned()))?;
        Self::connect_or_spawn_for(&target).await
    }

    /// Connect to the helper matching `target`, spawning `soyo helper` if needed.
    pub async fn connect_or_spawn_for(target: &HelperTarget) -> Result<Arc<Self>, DesktopError> {
        match Self::connect_for(target).await {
            Ok(client) => Ok(client),
            Err(first_err) => {
                spawn_helper_process(target)?;
                // Wait briefly for the daemon to bind its socket.
                for _ in 0..50 {
                    match Self::connect_for(target).await {
                        Ok(client) => return Ok(client),
                        Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                    }
                }
                Err(first_err)
            }
        }
    }

    /// Connect once to the default environment's helper; does not spawn anything.
    pub async fn connect() -> Result<Arc<Self>, DesktopError> {
        let target = HelperTarget::from_environment()
            .map_err(|err| DesktopError::new("invalid_config", err.message().to_owned()))?;
        Self::connect_for(&target).await
    }

    /// Connect once to the helper matching `target`; verifies private directory and peer UID.
    pub async fn connect_for(target: &HelperTarget) -> Result<Arc<Self>, DesktopError> {
        let helper_dir = target.helper_dir();
        if helper_dir.exists() {
            soyo_fs::open_existing_private_dir(&helper_dir, "helper_unauthorized", "helper")
                .map_err(|err| {
                    DesktopError::new("helper_unauthorized", err.message().to_owned())
                })?;
        }
        let socket_path = target.socket_path();
        let stream = UnixStream::connect(&socket_path).await.map_err(|err| {
            DesktopError::new(
                "helper_unavailable",
                format!("Cannot connect to soyo helper: {err}"),
            )
        })?;
        verify_peer_euid(&stream)?;
        Self::from_stream_with_target(stream, Some(target.clone())).await
    }

    /// Build a client over an already-connected stream (used by tests).
    pub async fn from_stream(stream: UnixStream) -> Result<Arc<Self>, DesktopError> {
        Self::from_stream_with_target(stream, None).await
    }

    async fn from_stream_with_target(
        stream: UnixStream,
        target: Option<HelperTarget>,
    ) -> Result<Arc<Self>, DesktopError> {
        let (mut reader, writer) = split(stream);
        let client = Arc::new(Self {
            writer: Mutex::new(writer),
            next_id: AtomicU64::new(1),
            pending: Mutex::new(PendingMap::default()),
            closed: AtomicBool::new(false),
            target,
        });
        // Handshake: first frame must be Hello.
        match read_server_frame(&mut reader).await.map_err(io_err)? {
            Some(ServerFrame::Hello { .. }) => {}
            other => {
                client.closed.store(true, Ordering::Release);
                return Err(DesktopError::new(
                    "helper_protocol",
                    format!("Expected hello frame, got {other:?}"),
                ));
            }
        }
        let pending_client = Arc::clone(&client);
        tokio::spawn(async move {
            pending_client.reader_loop(reader).await;
        });
        Ok(client)
    }

    /// Whether this connection has closed, shut down, or been detached.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Target configuration coordinates this client connected to, if known.
    pub fn target(&self) -> Option<&HelperTarget> {
        self.target.as_ref()
    }

    async fn reader_loop(&self, mut reader: crate::framing::FrameReader) {
        while let Ok(Some(frame)) = read_server_frame(&mut reader).await {
            self.dispatch(frame).await;
        }
        self.closed.store(true, Ordering::Release);
        self.fail_all_pending("helper_disconnected", "soyo helper disconnected")
            .await;
    }

    async fn dispatch(&self, frame: ServerFrame) {
        match frame {
            ServerFrame::Event { id, data } => {
                let sender = {
                    let pending = self.pending.lock().await;
                    match pending.entries.get(&id) {
                        Some(PendingEntry::Stream { events, .. }) => Some(events.clone()),
                        _ => None,
                    }
                };
                if let Some(events) = sender
                    && let Ok(env) = serde_json::from_value::<DesktopStreamEnvelope>(data)
                {
                    let _ = events.send(env);
                }
            }
            ServerFrame::Response { id, result } => {
                self.complete(id, Ok(result)).await;
            }
            ServerFrame::Error { id, code, message } => {
                self.complete(id, Err(DesktopError::new(code, message)))
                    .await;
            }
            ServerFrame::Bye => {
                self.closed.store(true, Ordering::Release);
                self.fail_all_pending("helper_shutdown", "soyo helper shut down")
                    .await;
            }
            ServerFrame::Hello { .. } => {}
        }
    }

    async fn complete(&self, id: u64, result: Result<Value, DesktopError>) {
        let entry = self.pending.lock().await.entries.remove(&id);
        if let Some(entry) = entry {
            let tx = match entry {
                PendingEntry::OneShot(tx) => tx,
                PendingEntry::Stream { result, .. } => result,
            };
            let _ = tx.send(result);
        }
    }

    async fn fail_all_pending(&self, code: &str, message: &str) {
        let entries = {
            let mut pending = self.pending.lock().await;
            std::mem::take(&mut pending.entries)
        };
        for (_, entry) in entries {
            let tx = match entry {
                PendingEntry::OneShot(tx) => tx,
                PendingEntry::Stream { result, .. } => result,
            };
            let _ = tx.send(Err(DesktopError::new(code, message)));
        }
    }

    async fn send_frame(&self, frame: &ClientFrame) -> Result<(), DesktopError> {
        if self.is_closed() {
            return Err(DesktopError::new(
                "helper_disconnected",
                "soyo helper connection is closed",
            ));
        }
        let mut writer = self.writer.lock().await;
        if let Err(err) = write_frame(&mut writer, frame).await {
            self.closed.store(true, Ordering::Release);
            drop(writer);
            self.fail_all_pending("helper_disconnected", "soyo helper write failed")
                .await;
            return Err(DesktopError::new(
                "helper_io",
                format!("helper write failed: {err}"),
            ));
        }
        Ok(())
    }

    fn register_pending(
        &self,
    ) -> (
        u64,
        PendingResult,
        oneshot::Receiver<Result<Value, DesktopError>>,
    ) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        (id, tx, rx)
    }

    /// One-shot request/response. Returns the raw JSON result.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, DesktopError> {
        let (id, tx, rx) = self.register_pending();
        {
            let mut pending = self.pending.lock().await;
            pending.entries.insert(id, PendingEntry::OneShot(tx));
        }
        if let Err(err) = self
            .send_frame(&ClientFrame::Request {
                id,
                method: method.to_owned(),
                params,
            })
            .await
        {
            self.pending.lock().await.entries.remove(&id);
            return Err(err);
        }
        rx.await.unwrap_or_else(|_| {
            Err(DesktopError::new(
                "helper_disconnected",
                "helper dropped the request",
            ))
        })
    }

    /// Typed one-shot call: params serialized from `P`, result into `R`.
    pub async fn call_typed<P: Serialize, R: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<R, DesktopError> {
        let params = serde_json::to_value(params)
            .map_err(|err| DesktopError::new("helper_protocol", format!("encode params: {err}")))?;
        let result = self.call(method, params).await?;
        serde_json::from_value(result)
            .map_err(|err| DesktopError::new("helper_protocol", format!("decode result: {err}")))
    }

    /// Streaming call: `DesktopStreamEnvelope` items arrive on `events` until
    /// the terminal response resolves the returned future.
    pub async fn call_streaming<P: Serialize, R: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
        events: mpsc::UnboundedSender<DesktopStreamEnvelope>,
    ) -> Result<R, DesktopError> {
        let (id, tx, rx) = self.register_pending();
        {
            let mut pending = self.pending.lock().await;
            pending
                .entries
                .insert(id, PendingEntry::Stream { result: tx, events });
        }
        let encoded = serde_json::to_value(params)
            .map_err(|err| DesktopError::new("helper_protocol", format!("encode params: {err}")))?;
        if let Err(err) = self
            .send_frame(&ClientFrame::Request {
                id,
                method: method.to_owned(),
                params: encoded,
            })
            .await
        {
            self.pending.lock().await.entries.remove(&id);
            return Err(err);
        }
        let result = rx.await.unwrap_or_else(|_| {
            Err(DesktopError::new(
                "helper_disconnected",
                "helper dropped the request",
            ))
        })?;
        serde_json::from_value(result)
            .map_err(|err| DesktopError::new("helper_protocol", format!("decode result: {err}")))
    }

    /// Best-effort cancel of an in-flight request on this connection.
    pub async fn cancel_request(&self, id: u64) {
        let _ = self.send_frame(&ClientFrame::CancelRequest { id }).await;
    }

    /// Ask the daemon to exit (does not wait for confirmation).
    pub async fn shutdown_daemon(&self) {
        let _ = self.send_frame(&ClientFrame::Shutdown).await;
    }

    /// Identity of the connected helper (version, protocol_version, pid, run/client counts).
    pub async fn helper_info(&self) -> Result<HelperInfoDto, DesktopError> {
        self.call_typed("helper_info", &serde_json::json!({})).await
    }

    /// Compare the running helper strictly by `version` and `protocol_version` (never
    /// by `exe_path`), and report whether active runs or other connected clients
    /// keep it busy.
    pub async fn helper_compat(&self) -> HelperCompat {
        match self.helper_info().await {
            Ok(info) => {
                let version_differs = info.version != env!("CARGO_PKG_VERSION");
                let protocol_differs = info.protocol_version != PROTOCOL_VERSION;
                let run_active = info.run_active || info.active_runs > 0;
                let other_clients = info.active_clients > 1;
                HelperCompat {
                    mismatch: version_differs || protocol_differs,
                    run_active,
                    busy: run_active || other_clients,
                    detail: format!(
                        "running v{} (proto {}), expected v{} (proto {}), runs={}, clients={}",
                        info.version,
                        info.protocol_version,
                        env!("CARGO_PKG_VERSION"),
                        PROTOCOL_VERSION,
                        info.active_runs,
                        info.active_clients,
                    ),
                }
            }
            Err(_) => {
                // Older helper without `helper_info`: stale by definition, but
                // still check for a busy run before recycling.
                let run_active = self
                    .call_typed::<_, serde_json::Value>(
                        "get_bootstrap_state",
                        &serde_json::json!({}),
                    )
                    .await
                    .ok()
                    .and_then(|boot| boot.get("run_active").and_then(|v| v.as_bool()))
                    .unwrap_or(false);
                HelperCompat {
                    mismatch: true,
                    run_active,
                    busy: run_active,
                    detail: "helper predates helper_info (stale daemon)".into(),
                }
            }
        }
    }

    /// Shut down the connected helper and establish a fresh connection for the
    /// same configuration target. Fails if the daemon does not exit or cannot serve.
    pub async fn restart(&self) -> Result<Arc<Self>, DesktopError> {
        let target = match &self.target {
            Some(target) => target.clone(),
            None => HelperTarget::from_environment()
                .map_err(|err| DesktopError::new("invalid_config", err.message().to_owned()))?,
        };
        self.shutdown_daemon().await;
        for _ in 0..50 {
            if UnixStream::connect(target.socket_path()).await.is_err() {
                return Self::connect_or_spawn_for(&target).await;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(DesktopError::new(
            "helper_restart",
            "helper did not exit after shutdown",
        ))
    }

    /// Close the client half of the connection. Pending requests fail fast
    /// instead of hanging on the reader task.
    pub async fn detach(&self) {
        self.closed.store(true, Ordering::Release);
        self.fail_all_pending("client_detached", "client detached from helper")
            .await;
    }
}

fn io_err(err: std::io::Error) -> DesktopError {
    DesktopError::new("helper_io", format!("helper io failed: {err}"))
}

/// Binary used to spawn the helper. Resolution order:
/// `SOYO_HELPER_BIN` → sibling of the current executable (bundled/sidecar
/// installs) → `soyo` on PATH.
fn helper_bin() -> PathBuf {
    if let Ok(bin) = std::env::var("SOYO_HELPER_BIN")
        && !bin.trim().is_empty()
    {
        return PathBuf::from(bin);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        for name in ["soyo", "soyo.exe"] {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    PathBuf::from("soyo")
}

fn spawn_helper_process(target: &HelperTarget) -> Result<(), DesktopError> {
    let mut cmd = std::process::Command::new(helper_bin());
    cmd.arg("--config")
        .arg(&target.config_path)
        .arg("--data-dir")
        .arg(&target.data_dir)
        .arg("helper")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: `pre_exec` runs between `fork` and `exec` using only async-signal-safe
    // POSIX calls (`setsid`, `getrlimit`, `fcntl`) to detach from any caller PTY
    // and mark extra inherited FDs `FD_CLOEXEC` (leaving stdio and Rust's exec
    // error pipe intact until `execvp` replaces the process image).
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mut limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            let max_fd = if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) == 0
                && limit.rlim_cur != libc::RLIM_INFINITY
            {
                limit.rlim_cur.min(10_240) as libc::c_int
            } else {
                4096
            };
            for fd in 3..max_fd {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags >= 0 && (flags & libc::FD_CLOEXEC) == 0 {
                    let _ = libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
                }
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(|err| {
        DesktopError::new(
            "helper_spawn",
            format!("Failed to start `soyo helper` ({err}); is `soyo` on PATH?"),
        )
    })?;
    // Wait in a dedicated daemon-reaper thread so long-lived desktop apps never
    // accumulate exited helper zombie processes.
    let _ = std::thread::Builder::new()
        .name("soyo-helper-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        });
    Ok(())
}
