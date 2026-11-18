//! NDJSON socket server: accepts local-socket clients, multiplexes requests onto
//! `HostService`, and streams `DesktopStreamEnvelope` events back per request id.

use interprocess::local_socket::{
    ListenerOptions,
    tokio::{Stream as LocalStream, prelude::*},
};
use serde::Serialize;
use serde_json::Value;
use soyo_ipc::{
    framing::{FrameWriter, read_client_frame, split, write_frame},
    protocol::{ClientFrame, PROTOCOL_VERSION, ServerFrame},
    socket::{HelperEndpoint, helper_pid_path},
    types::{DesktopError, DesktopStreamEnvelope},
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, mpsc};

use crate::{
    attachment, prefs,
    service::{HostService, StreamSink},
};

pub struct ServerConfig {
    /// Whether to write/remove the pid file alongside the socket.
    pub manage_pid_file: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            manage_pid_file: true,
        }
    }
}

pub struct HelperServer {
    service: Arc<HostService>,
    shutdown: Arc<AtomicBool>,
}

impl HelperServer {
    pub fn new(service: HostService) -> Self {
        Self {
            service: Arc::new(service),
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn service(&self) -> Arc<HostService> {
        Arc::clone(&self.service)
    }

    /// Serve until `ClientFrame::Shutdown`, `service.shutdown()`, or listener error.
    pub async fn run(&self) -> Result<(), DesktopError> {
        let name = HelperEndpoint::name();
        // Remove a stale socket file so bind does not fail after a crash.
        if let Some(path) = HelperEndpoint::socket_path() {
            let _ = std::fs::remove_file(&path);
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        let listener = ListenerOptions::new()
            .name(name)
            .create_tokio()
            .map_err(|err| {
                DesktopError::new("helper_bind", format!("Cannot bind helper socket: {err}"))
            })?;

        if self.manage_pid() {
            write_pid_file();
        }

        // Watchdog: the bound socket lives at a filesystem path; if that path
        // is unlinked (tempdir cleanup, `soyo kill` racing a stale bind) or
        // rebound by another helper, this instance can never be reached again
        // and must exit instead of leaking as an orphan.
        if let Some(path) = HelperEndpoint::socket_path()
            && let Some(bound) = socket_file_identity(&path)
        {
            let shutdown = Arc::clone(&self.shutdown);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(SOCKET_WATCHDOG_INTERVAL).await;
                    if socket_file_identity(&path) != Some(bound) {
                        shutdown.store(true, Ordering::Relaxed);
                        return;
                    }
                }
            });
        }

        let service = Arc::clone(&self.service);
        while !self.shutdown.load(Ordering::Relaxed) {
            let accept = tokio::select! {
                accept = listener.accept() => accept,
                _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => continue,
            };
            match accept {
                Ok(stream) => {
                    let svc = Arc::clone(&service);
                    let shutdown = Arc::clone(&self.shutdown);
                    tokio::spawn(async move {
                        handle_client(stream, svc, shutdown).await;
                    });
                }
                Err(err) => {
                    if self.shutdown.load(Ordering::Relaxed) {
                        break;
                    }
                    eprintln!("soyo helper: accept failed: {err}");
                }
            }
        }

        self.service.shutdown().await;
        if self.manage_pid() {
            let _ = std::fs::remove_file(helper_pid_path());
        }
        Ok(())
    }

    fn manage_pid(&self) -> bool {
        // Pid file is advisory; keep it for `soyo kill` diagnostics.
        true
    }
}

fn write_pid_file() {
    let path = helper_pid_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, format!("{}\n", std::process::id()));
}

const SOCKET_WATCHDOG_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Identity of the bound socket file: `(dev, ino)` on Unix, existence-only
/// elsewhere. `None` when the path is gone — treated as "socket removed".
#[cfg(unix)]
fn socket_file_identity(path: &std::path::Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn socket_file_identity(path: &std::path::Path) -> Option<()> {
    std::fs::metadata(path).ok().map(|_| ())
}

async fn handle_client(stream: LocalStream, service: Arc<HostService>, shutdown: Arc<AtomicBool>) {
    let (mut reader, writer) = split(stream);
    let writer = Arc::new(Mutex::new(writer));
    let hello = ServerFrame::Hello {
        version: PROTOCOL_VERSION,
        pid: std::process::id(),
    };
    {
        let mut w = writer.lock().await;
        if write_frame(&mut w, &hello).await.is_err() {
            return;
        }
    }
    let mut streaming_ids: Vec<u64> = Vec::new();
    loop {
        if shutdown.load(Ordering::Relaxed) {
            let mut w = writer.lock().await;
            let _ = write_frame(&mut w, &ServerFrame::Bye).await;
            break;
        }
        match read_client_frame(&mut reader).await {
            Ok(Some(frame)) => match frame {
                ClientFrame::Request { id, method, params } => {
                    streaming_ids.push(id);
                    let svc = Arc::clone(&service);
                    let w = Arc::clone(&writer);
                    let sd = Arc::clone(&shutdown);
                    tokio::spawn(async move {
                        dispatch(id, method, params, svc, w, sd).await;
                    });
                }
                ClientFrame::CancelRequest { id } => {
                    // Streaming cancel maps onto run cancellation.
                    service.cancel_run();
                    let _ = id;
                }
                ClientFrame::Shutdown => {
                    shutdown.store(true, Ordering::Relaxed);
                    service.shutdown().await;
                    let mut w = writer.lock().await;
                    let _ = write_frame(&mut w, &ServerFrame::Bye).await;
                    break;
                }
            },
            Ok(None) | Err(_) => break,
        }
    }
    let _ = streaming_ids;
}

fn err_frame(id: u64, err: DesktopError) -> ServerFrame {
    ServerFrame::Error {
        id,
        code: err.code,
        message: err.message,
    }
}

async fn send_frame(writer: &Mutex<FrameWriter>, frame: &ServerFrame) {
    let mut w = writer.lock().await;
    let _ = write_frame(&mut w, frame).await;
}

fn param<'a>(params: &'a Value, key: &str) -> Result<&'a Value, DesktopError> {
    params
        .get(key)
        .ok_or_else(|| DesktopError::new("invalid_params", format!("Missing param '{key}'")))
}

fn param_opt<'a>(params: &'a Value, key: &str) -> Option<&'a Value> {
    params.get(key).filter(|v| !v.is_null())
}

fn param_string<'a>(params: &'a Value, key: &str) -> Result<&'a str, DesktopError> {
    param(params, key)?.as_str().ok_or_else(|| {
        DesktopError::new("invalid_params", format!("Param '{key}' must be a string"))
    })
}

fn param_bool(params: &Value, key: &str) -> Result<bool, DesktopError> {
    param(params, key)?
        .as_bool()
        .ok_or_else(|| DesktopError::new("invalid_params", format!("Param '{key}' must be a bool")))
}

fn param_deser<T: serde::de::DeserializeOwned>(
    params: &Value,
    key: &str,
) -> Result<T, DesktopError> {
    serde_json::from_value(param(params, key)?.clone())
        .map_err(|err| DesktopError::new("invalid_params", format!("Param '{key}': {err}")))
}

fn ok(value: impl Serialize) -> Result<Value, DesktopError> {
    serde_json::to_value(value)
        .map_err(|err| DesktopError::new("internal_error", format!("encode result: {err}")))
}

async fn dispatch(
    id: u64,
    method: String,
    params: Value,
    service: Arc<HostService>,
    writer: Arc<Mutex<FrameWriter>>,
    _shutdown: Arc<AtomicBool>,
) {
    let result = match method.as_str() {
        "get_bootstrap_state" => ok(service.get_bootstrap_state().await),
        "list_sessions_page" => {
            let page = param_opt(&params, "page")
                .and_then(Value::as_u64)
                .map(|v| v as u32);
            service.list_sessions_page(page).await.and_then(ok)
        }
        "new_session" => service.new_session().await.and_then(ok),
        "resume_session" => {
            let r = async {
                let session_id = param_string(&params, "session_id")?.to_owned();
                let accept = param_bool(&params, "accept_config_change")?;
                service.resume_session(&session_id, accept).await
            };
            r.await.and_then(ok)
        }
        "cancel_run" => ok(service.cancel_run()),
        "reply_approval" => {
            let r = (|| {
                service.reply_approval(
                    param_string(&params, "approval_id")?,
                    param_string(&params, "call_id")?,
                    param_bool(&params, "approved")?,
                )
            })();
            r.and_then(|_| ok(Value::Null))
        }
        "reply_input" => {
            let r = (|| {
                service.reply_input(
                    param_string(&params, "input_id")?,
                    param_opt(&params, "answer")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                )
            })();
            r.and_then(|_| ok(Value::Null))
        }
        "set_work_mode" => {
            let r = async {
                let mode = param_deser(&params, "mode")?;
                service.set_work_mode(mode).await
            };
            r.await.and_then(ok)
        }
        "set_permission_mode" => {
            let r = async {
                let permissions = param_deser(&params, "permissions")?;
                service.set_permission_mode(permissions).await
            };
            r.await.and_then(ok)
        }
        "set_workspace" => {
            let r = async {
                let path = param_string(&params, "path")?.to_owned();
                service.set_workspace(&path).await
            };
            r.await.and_then(ok)
        }
        "select_profile" => {
            let r = async {
                let profile = param_string(&params, "profile")?.to_owned();
                service.select_profile(&profile).await
            };
            r.await.and_then(ok)
        }
        "set_thinking_effort" => {
            let effort = param_opt(&params, "effort")
                .and_then(Value::as_str)
                .map(str::to_owned);
            service.set_thinking_effort(effort).await.and_then(ok)
        }
        "set_sandbox_backend" => {
            let r = async {
                let backend = param_deser(&params, "backend")?;
                service.set_sandbox_backend(backend).await
            };
            r.await.and_then(ok)
        }
        "set_autocompact" => {
            let r = async {
                let enabled = param_bool(&params, "enabled")?;
                let trigger = param_opt(&params, "trigger_tokens").and_then(Value::as_u64);
                service.set_autocompact(enabled, trigger).await
            };
            r.await.and_then(ok)
        }
        "list_extensions" => service.list_extensions().await.and_then(ok),
        "list_jobs" => service.list_jobs().await.and_then(ok),
        "cancel_background_job" => {
            let r = (|| service.cancel_background_job(param_string(&params, "job_id")?))();
            r.and_then(ok)
        }
        "list_modules" => service.list_modules().await.and_then(ok),
        "set_module_enabled" => {
            let r = async {
                let module = param_string(&params, "module")?.to_owned();
                let enabled = param_bool(&params, "enabled")?;
                service.set_module_enabled(&module, enabled).await
            };
            r.await.and_then(ok)
        }
        "list_mcp_servers" => service.list_mcp_servers().await.and_then(ok),
        "toggle_mcp_server" => {
            let r = async {
                let name = param_string(&params, "name")?.to_owned();
                let enabled = param_bool(&params, "enabled")?;
                service.toggle_mcp_server(&name, enabled).await
            };
            r.await.and_then(ok)
        }
        "list_credentials" => service.list_credentials().await.and_then(ok),
        "import_credential" => {
            let r = async {
                let server = param_string(&params, "server")?.to_owned();
                let env_var = param_string(&params, "env_var")?.to_owned();
                let scopes = param_deser::<Vec<String>>(&params, "scopes")?;
                let approve = param_bool(&params, "approve_scopes")?;
                service
                    .import_credential(&server, &env_var, scopes, approve)
                    .await
            };
            r.await.and_then(ok)
        }
        "revoke_credential" => {
            let r = async {
                let reference = param_string(&params, "reference")?.to_owned();
                service.revoke_credential(&reference).await
            };
            r.await.and_then(ok)
        }
        "query_memory" => {
            let r = async {
                let command = param_string(&params, "command")?.to_owned();
                service.query_memory(&command).await
            };
            r.await.and_then(ok)
        }
        "prepare_memory_action" => {
            let r = async {
                let command = param_string(&params, "command")?.to_owned();
                service.prepare_memory_action(&command).await
            };
            r.await.and_then(ok)
        }
        "confirm_memory_action" => {
            let r = async {
                let action_id = param_string(&params, "action_id")?.to_owned();
                service.confirm_memory_action(&action_id).await
            };
            r.await.and_then(ok)
        }
        "export_session" => {
            let r = async {
                let session_id = param_opt(&params, "session_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let target_path = param_string(&params, "target_path")?.to_owned();
                let format = param_deser(&params, "format")?;
                let scope = param_deser(&params, "scope")?;
                let include_tools = param_bool(&params, "include_tools")?;
                service
                    .export_session(session_id, &target_path, format, scope, include_tools)
                    .await
            };
            r.await.and_then(ok)
        }
        "stage_attachments" => {
            let r = async {
                let paths = param_deser::<Vec<String>>(&params, "paths")?;
                service.stage_attachments(paths).await
            };
            r.await.and_then(ok)
        }
        "get_desktop_prefs" => prefs::get_desktop_prefs().await.and_then(ok),
        "set_nickname" => {
            let nickname = param_opt(&params, "nickname")
                .and_then(Value::as_str)
                .map(str::to_owned);
            prefs::set_nickname(nickname).await.and_then(ok)
        }
        "set_avatar" => {
            let source = param_opt(&params, "source_path")
                .and_then(Value::as_str)
                .map(str::to_owned);
            prefs::set_avatar(source).await.and_then(ok)
        }
        "get_user_name" => prefs::get_user_name().await.and_then(ok),
        "get_global_prompt" => prefs::get_global_prompt().await.and_then(ok),
        "set_global_prompt" => {
            let r = async {
                let content = param_string(&params, "content")?.to_owned();
                prefs::set_global_prompt(content).await
            };
            r.await.and_then(ok)
        }
        "fetch_attachment" => (|| {
            let method = param_string(&params, "method")?;
            let uri = param_string(&params, "uri")?;
            let payload = attachment::resolve_attachment_request(
                service.attachment_scope.as_ref(),
                &service.staged_drafts,
                method,
                uri,
            );
            use base64::{Engine, engine::general_purpose::STANDARD};
            ok(soyo_ipc::types::AttachmentPayloadDto {
                status: payload.status,
                content_type: payload.content_type.to_owned(),
                body_b64: STANDARD.encode(&payload.body),
            })
        })(),
        "submit_prompt" | "compact_session" | "run_prompt" => {
            dispatch_streaming(id, &method, params, &service, &writer).await
        }
        _ => Err(DesktopError::new(
            "unknown_method",
            format!("Unknown helper method '{method}'"),
        )),
    };

    let frame = match result {
        Ok(value) => ServerFrame::Response { id, result: value },
        Err(err) => err_frame(id, err),
    };
    send_frame(&writer, &frame).await;
}

async fn dispatch_streaming(
    id: u64,
    method: &str,
    params: Value,
    service: &Arc<HostService>,
    writer: &Arc<Mutex<FrameWriter>>,
) -> Result<Value, DesktopError> {
    let (events_tx, mut events_rx) = mpsc::unbounded_channel::<DesktopStreamEnvelope>();
    let writer_for_events = Arc::clone(writer);
    let forwarder = tokio::spawn(async move {
        while let Some(env) = events_rx.recv().await {
            let frame = ServerFrame::Event {
                id,
                data: serde_json::to_value(&env).unwrap_or(Value::Null),
            };
            send_frame(&writer_for_events, &frame).await;
        }
    });
    let sink = StreamSink::Mpsc(events_tx);
    let result = match method {
        "submit_prompt" => {
            let prompt = param_string(&params, "prompt")?.to_owned();
            let attachments = param_opt(&params, "attachments")
                .cloned()
                .map(|v| {
                    serde_json::from_value(v).map_err(|err| {
                        DesktopError::new("invalid_params", format!("attachments: {err}"))
                    })
                })
                .transpose()?
                .unwrap_or_default();
            service.submit_prompt(prompt, attachments, sink).await
        }
        "run_prompt" => {
            let run_params: soyo_ipc::types::RunPromptParams = serde_json::from_value(params)
                .map_err(|err| {
                    DesktopError::new("invalid_params", format!("run_prompt params: {err}"))
                })?;
            service.run_prompt(run_params, sink).await
        }
        _ => service.compact_session(sink).await,
    };
    let _ = forwarder.await;
    result.and_then(ok)
}
