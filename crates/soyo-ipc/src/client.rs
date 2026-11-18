//! Client for the `soyo helper` daemon: connect (auto-spawning if needed),
//! multiplexed request/response, streaming subscriptions, and shutdown.

use interprocess::local_socket::tokio::{Stream, prelude::*};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::HashMap,
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::{
    framing::{FrameWriter, read_server_frame, split, write_frame},
    protocol::{ClientFrame, ServerFrame},
    socket::HelperEndpoint,
    types::{DesktopError, DesktopStreamEnvelope},
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

/// Shared client handle. Clone-free use via `Arc` inside managed state.
pub struct HelperClient {
    writer: Mutex<FrameWriter>,
    next_id: AtomicU64,
    pending: Mutex<PendingMap>,
}

impl HelperClient {
    /// Connect to a running helper, spawning `soyo helper` if none is listening.
    pub async fn connect_or_spawn() -> Result<Arc<Self>, DesktopError> {
        match Self::connect().await {
            Ok(client) => Ok(client),
            Err(first_err) => {
                spawn_helper_process()?;
                // Wait briefly for the daemon to bind its socket.
                for _ in 0..50 {
                    match Self::connect().await {
                        Ok(client) => return Ok(client),
                        Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                    }
                }
                Err(first_err)
            }
        }
    }

    /// Connect once; does not spawn anything.
    pub async fn connect() -> Result<Arc<Self>, DesktopError> {
        let stream = Stream::connect(HelperEndpoint::name())
            .await
            .map_err(|err| {
                DesktopError::new(
                    "helper_unavailable",
                    format!("Cannot connect to soyo helper: {err}"),
                )
            })?;
        Self::from_stream(stream).await
    }

    /// Build a client over an already-connected stream (used by tests).
    pub async fn from_stream(stream: Stream) -> Result<Arc<Self>, DesktopError> {
        let (mut reader, writer) = split(stream);
        let client = Arc::new(Self {
            writer: Mutex::new(writer),
            next_id: AtomicU64::new(1),
            pending: Mutex::new(PendingMap::default()),
        });
        // Handshake: first frame must be Hello.
        match read_server_frame(&mut reader).await.map_err(io_err)? {
            Some(ServerFrame::Hello { .. }) => {}
            other => {
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

    async fn reader_loop(&self, mut reader: crate::framing::FrameReader) {
        while let Ok(Some(frame)) = read_server_frame(&mut reader).await {
            self.dispatch(frame).await;
        }
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
        let mut writer = self.writer.lock().await;
        write_frame(&mut writer, frame)
            .await
            .map_err(|err| DesktopError::new("helper_io", format!("helper write failed: {err}")))
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
        self.send_frame(&ClientFrame::Request {
            id,
            method: method.to_owned(),
            params,
        })
        .await?;
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
        self.send_frame(&ClientFrame::Request {
            id,
            method: method.to_owned(),
            params: serde_json::to_value(params).map_err(|err| {
                DesktopError::new("helper_protocol", format!("encode params: {err}"))
            })?,
        })
        .await?;
        let result = rx.await.unwrap_or_else(|_| {
            Err(DesktopError::new(
                "helper_disconnected",
                "helper dropped the request",
            ))
        })?;
        serde_json::from_value(result)
            .map_err(|err| DesktopError::new("helper_protocol", format!("decode result: {err}")))
    }

    /// Best-effort cancel of an in-flight request.
    pub async fn cancel_request(&self, id: u64) {
        let _ = self.send_frame(&ClientFrame::CancelRequest { id }).await;
    }

    /// Ask the daemon to exit (does not wait for confirmation).
    pub async fn shutdown_daemon(&self) {
        let _ = self.send_frame(&ClientFrame::Shutdown).await;
    }

    /// Close the client half of the connection. Pending requests fail fast
    /// instead of hanging on the reader task.
    pub async fn detach(&self) {
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

fn spawn_helper_process() -> Result<(), DesktopError> {
    let mut cmd = std::process::Command::new(helper_bin());
    cmd.arg("helper")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn().map_err(|err| {
        DesktopError::new(
            "helper_spawn",
            format!("Failed to start `soyo helper` ({err}); is `soyo` on PATH?"),
        )
    })?;
    Ok(())
}
