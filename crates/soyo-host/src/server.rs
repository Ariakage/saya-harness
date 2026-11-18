//! NDJSON socket server: accepts verified local-socket clients, multiplexes
//! requests onto `HostService` with connection/request ownership tracking, and
//! streams `DesktopStreamEnvelope` events back per request id.

use serde::Serialize;
use serde_json::Value;
use soyo_ipc::{
    framing::{FrameWriter, read_client_frame, split, write_frame},
    protocol::{ClientFrame, PROTOCOL_VERSION, ServerFrame},
    socket::{
        HelperPidRecord, HelperTarget, process_exe_path, process_started_id, verify_peer_euid,
    },
    types::{DesktopError, DesktopStreamEnvelope},
};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::{UnixListener, UnixStream},
    sync::{Mutex, mpsc},
};

use crate::{
    attachment, prefs,
    service::{ClientOwner, HostService, StreamSink},
};

static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(1);

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
        let target = self.service.helper_target().await;
        crate::envfile::migrate_legacy_env(&target.state_dir);

        if !HelperTarget::uses_env_helper_dir() {
            soyo_fs::ensure_root_dir(&target.state_dir, "helper_bind", "state")
                .map_err(|err| DesktopError::new("helper_bind", err.message().to_owned()))?;
        }
        let helper_dir = target.helper_dir();
        soyo_fs::ensure_private_dir(&helper_dir, "helper_bind", "helper")
            .map_err(|err| DesktopError::new("helper_bind", err.message().to_owned()))?;

        // Coordinate bind/reclaim across racing helper launches via an exclusive flock.
        let lock_path = target.lock_path();
        let _lock = soyo_fs::acquire_flock(
            &lock_path,
            Duration::from_secs(2),
            "helper_already_running",
            "helper_bind",
            "helper",
        )
        .map_err(|err| DesktopError::new(err.code(), err.message().to_owned()))?;

        let socket_path = target.socket_path();
        match fs::symlink_metadata(&socket_path) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(DesktopError::new(
                        "helper_bind",
                        "Refusing symlink at helper socket path",
                    ));
                }
                // Check whether a live helper is already serving on this socket before unlinking.
                if let Ok(Ok(stream)) = tokio::time::timeout(
                    Duration::from_millis(200),
                    UnixStream::connect(&socket_path),
                )
                .await
                    && verify_peer_euid(&stream).is_ok()
                {
                    return Err(DesktopError::new(
                        "helper_already_running",
                        "Another helper for this configuration is already running",
                    ));
                }
                let _ = fs::remove_file(&socket_path);
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(DesktopError::new(
                    "helper_bind",
                    format!("Cannot inspect helper socket path: {err}"),
                ));
            }
        }

        let listener = UnixListener::bind(&socket_path).map_err(|err| {
            DesktopError::new("helper_bind", format!("Cannot bind helper socket: {err}"))
        })?;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600)).map_err(|err| {
            let _ = fs::remove_file(&socket_path);
            DesktopError::new(
                "helper_bind",
                format!("Cannot set 0600 permissions on helper socket: {err}"),
            )
        })?;

        let pid_record = if self.manage_pid() {
            write_pid_file(&target)
        } else {
            None
        };

        // Watchdog: the bound socket lives at a filesystem path; if that path
        // is unlinked (tempdir cleanup, `soyo kill` racing a stale bind) or
        // rebound by another helper, this instance can never be reached again
        // and must exit instead of leaking as an orphan.
        let bound_identity = socket_file_identity(&socket_path);
        if let Some(bound) = bound_identity {
            let watch_path = socket_path.clone();
            let shutdown = Arc::clone(&self.shutdown);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(SOCKET_WATCHDOG_INTERVAL).await;
                    if socket_file_identity(&watch_path) != Some(bound) {
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
                _ = tokio::time::sleep(Duration::from_millis(200)) => continue,
            };
            match accept {
                Ok((stream, _addr)) => {
                    if let Err(err) = verify_peer_euid(&stream) {
                        eprintln!("soyo helper: rejected client: {}", err.message);
                        continue;
                    }
                    let conn_id = NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed);
                    let svc = Arc::clone(&service);
                    let shutdown = Arc::clone(&self.shutdown);
                    svc.active_clients.fetch_add(1, Ordering::AcqRel);
                    tokio::spawn(async move {
                        handle_client(conn_id, stream, svc, shutdown).await;
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
        if let Some(record) = pid_record {
            remove_pid_file_if_owned(&target.pid_path(), &record);
        }
        if bound_identity.is_some() && socket_file_identity(&socket_path) == bound_identity {
            let _ = fs::remove_file(&socket_path);
        }
        Ok(())
    }

    fn manage_pid(&self) -> bool {
        true
    }
}

fn current_pid_record(target: &HelperTarget) -> HelperPidRecord {
    let pid = std::process::id();
    let started_at_id = process_started_id(pid).unwrap_or(0);
    let exe_path = process_exe_path(pid)
        .or_else(|| std::env::current_exe().ok())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    HelperPidRecord {
        pid,
        started_at_id,
        exe_path,
        fingerprint: target.fingerprint.clone(),
    }
}

fn write_pid_file(target: &HelperTarget) -> Option<HelperPidRecord> {
    let path = target.pid_path();
    let record = current_pid_record(target);
    let encoded = serde_json::to_vec(&record).ok()?;
    let tmp = path.with_file_name(format!("{}.{}.pid.tmp", target.fingerprint, record.pid));
    let _ = fs::remove_file(&tmp);
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = options.open(&tmp).ok()?;
    file.write_all(&encoded).ok()?;
    drop(file);
    fs::rename(&tmp, &path).ok()?;
    Some(record)
}

fn remove_pid_file_if_owned(pid_path: &Path, expected: &HelperPidRecord) {
    let Ok(Some((file, _))) =
        soyo_fs::open_existing_private_file_read(pid_path, "helper_pid", "helper pid")
    else {
        return;
    };
    let mut raw = String::new();
    if file.take(16 * 1024).read_to_string(&mut raw).is_err() {
        return;
    }
    if let Ok(found) = serde_json::from_str::<HelperPidRecord>(&raw)
        && found.pid == expected.pid
        && found.started_at_id == expected.started_at_id
        && found.fingerprint == expected.fingerprint
    {
        let _ = fs::remove_file(pid_path);
    }
}

const SOCKET_WATCHDOG_INTERVAL: Duration = Duration::from_secs(1);

fn socket_file_identity(path: &Path) -> Option<(u64, u64)> {
    let meta = fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_symlink() {
        return None;
    }
    Some((meta.dev(), meta.ino()))
}

async fn handle_client(
    conn_id: u64,
    stream: UnixStream,
    service: Arc<HostService>,
    shutdown: Arc<AtomicBool>,
) {
    let (mut reader, writer) = split(stream);
    let writer = Arc::new(Mutex::new(writer));
    let hello = ServerFrame::Hello {
        version: PROTOCOL_VERSION,
        pid: std::process::id(),
    };
    {
        let mut w = writer.lock().await;
        if write_frame(&mut w, &hello).await.is_err() {
            service.active_clients.fetch_sub(1, Ordering::AcqRel);
            return;
        }
    }
    loop {
        if shutdown.load(Ordering::Relaxed) {
            let mut w = writer.lock().await;
            let _ = write_frame(&mut w, &ServerFrame::Bye).await;
            break;
        }
        match read_client_frame(&mut reader).await {
            Ok(Some(frame)) => match frame {
                ClientFrame::Request { id, method, params } => {
                    let svc = Arc::clone(&service);
                    let w = Arc::clone(&writer);
                    let sd = Arc::clone(&shutdown);
                    tokio::spawn(async move {
                        dispatch(conn_id, id, method, params, svc, w, sd).await;
                    });
                }
                ClientFrame::CancelRequest { id } => {
                    service.cancel_request(conn_id, id);
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
    service.active_clients.fetch_sub(1, Ordering::AcqRel);
    service.on_client_disconnected(conn_id).await;
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

fn param_opt_str<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    param_opt(params, key).and_then(Value::as_str)
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
    conn_id: u64,
    id: u64,
    method: String,
    params: Value,
    service: Arc<HostService>,
    writer: Arc<Mutex<FrameWriter>>,
    _shutdown: Arc<AtomicBool>,
) {
    let owner = ClientOwner {
        conn_id,
        request_id: id,
    };
    let result = match method.as_str() {
        "get_bootstrap_state" => {
            let sid = param_opt_str(&params, "session_id");
            ok(service.get_bootstrap_state_for(conn_id, sid).await)
        }
        "helper_info" => ok(service.helper_info().await),
        "list_sessions_page" => {
            let page = param_opt(&params, "page")
                .and_then(Value::as_u64)
                .map(|v| v as u32);
            service.list_sessions_page(page).await.and_then(ok)
        }
        "usage_stats" => {
            let days = param_opt(&params, "days")
                .and_then(Value::as_u64)
                .map(|v| v as u32);
            let utc_offset = param_opt(&params, "utc_offset_minutes")
                .and_then(Value::as_i64)
                .and_then(|v| i32::try_from(v).ok());
            service.usage_stats(days, utc_offset).await.and_then(ok)
        }
        "new_session" => {
            let r = async {
                let cwd = param_opt_str(&params, "cwd").map(str::to_owned);
                let profile = param_opt_str(&params, "profile").map(str::to_owned);
                service.new_session_for(conn_id, cwd, profile).await
            };
            r.await.and_then(ok)
        }
        "resume_session" => {
            let r = async {
                let resume: soyo_ipc::types::ResumeSessionDto = serde_json::from_value(params)
                    .map_err(|err| {
                        DesktopError::new("invalid_params", format!("resume_session params: {err}"))
                    })?;
                service.resume_session_for(conn_id, &resume).await
            };
            r.await.and_then(ok)
        }
        "release_session" => {
            let sid = param_opt_str(&params, "session_id");
            service.release_session_for(conn_id, sid).await.and_then(ok)
        }
        "get_run_state" => {
            let sid = param_opt_str(&params, "session_id");
            service.get_run_state_for(conn_id, sid).await.and_then(ok)
        }
        "get_job" => {
            let r = async {
                let job_id = param_string(&params, "job_id")?;
                let sid = param_opt_str(&params, "session_id");
                service.get_job_for(conn_id, sid, job_id).await
            };
            r.await.and_then(ok)
        }
        "get_session" => {
            let r = async {
                let session_id = param_string(&params, "session_id")?;
                service.get_session(session_id).await
            };
            r.await.and_then(ok)
        }
        "read_session_snapshot" => {
            let r = async {
                let session_id = param_string(&params, "session_id")?;
                service.read_session_snapshot(session_id).await
            };
            r.await.and_then(ok)
        }
        "cancel_job" => {
            let r = async {
                let job_id = param_string(&params, "job_id")?;
                service.cancel_job_for(conn_id, job_id).await
            };
            r.await.and_then(ok)
        }
        "cancel_run" => {
            let run_id = param_opt_str(&params, "run_id");
            ok(service.cancel_run_for(conn_id, run_id))
        }
        "reply_approval" => {
            let r = (|| {
                service.reply_approval_for(
                    conn_id,
                    param_string(&params, "approval_id")?,
                    param_string(&params, "call_id")?,
                    param_bool(&params, "approved")?,
                )
            })();
            r.and_then(|_| ok(Value::Null))
        }
        "reply_input" => {
            let r = (|| {
                service.reply_input_for(
                    conn_id,
                    param_string(&params, "input_id")?,
                    param_opt_str(&params, "answer").map(str::to_owned),
                )
            })();
            r.and_then(|_| ok(Value::Null))
        }
        "set_work_mode" => {
            let r = async {
                let mode = param_deser(&params, "mode")?;
                let sid = param_opt_str(&params, "session_id");
                service.set_work_mode_for(conn_id, sid, mode).await
            };
            r.await.and_then(ok)
        }
        "set_permission_mode" => {
            let r = async {
                let permissions = param_deser(&params, "permissions")?;
                let sid = param_opt_str(&params, "session_id");
                service
                    .set_permission_mode_for(conn_id, sid, permissions)
                    .await
            };
            r.await.and_then(ok)
        }
        "set_workspace" => {
            let r = async {
                let path = param_string(&params, "path")?.to_owned();
                let sid = param_opt_str(&params, "session_id");
                service.set_workspace_for(conn_id, sid, &path).await
            };
            r.await.and_then(ok)
        }
        "setup_environment" => {
            let r = async {
                let setup: soyo_ipc::types::SetupEnvironmentParams = serde_json::from_value(params)
                    .map_err(|err| {
                        DesktopError::new(
                            "invalid_params",
                            format!("setup_environment params: {err}"),
                        )
                    })?;
                service.setup_environment_for(conn_id, setup).await
            };
            r.await.and_then(ok)
        }
        "select_profile" => {
            let r = async {
                let profile = param_string(&params, "profile")?.to_owned();
                let sid = param_opt_str(&params, "session_id");
                service.select_profile_for(conn_id, sid, &profile).await
            };
            r.await.and_then(ok)
        }
        "set_thinking_effort" => {
            let effort = param_opt_str(&params, "effort").map(str::to_owned);
            let sid = param_opt_str(&params, "session_id");
            service
                .set_thinking_effort_for(conn_id, sid, effort)
                .await
                .and_then(ok)
        }
        "set_sandbox_backend" => {
            let r = async {
                let backend = param_deser(&params, "backend")?;
                service.set_sandbox_backend(backend).await
            };
            r.await.and_then(ok)
        }
        "set_memory_setting" => {
            let r = async {
                let setting = param_string(&params, "setting")?.to_owned();
                let enabled = param_bool(&params, "enabled")?;
                service.set_memory_setting(&setting, enabled).await
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
        "list_jobs" => {
            let sid = param_opt_str(&params, "session_id");
            service.list_jobs_for(conn_id, sid).await.and_then(ok)
        }
        "cancel_background_job" => {
            let r =
                (|| service.cancel_background_job_for(conn_id, param_string(&params, "job_id")?))();
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
                let session_id = param_opt_str(&params, "session_id").map(str::to_owned);
                let target_path = param_string(&params, "target_path")?.to_owned();
                let format = param_deser(&params, "format")?;
                let scope = param_deser(&params, "scope")?;
                let include_tools = param_bool(&params, "include_tools")?;
                service
                    .export_session_for(
                        conn_id,
                        session_id,
                        &target_path,
                        format,
                        scope,
                        include_tools,
                    )
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
            let nickname = param_opt_str(&params, "nickname").map(str::to_owned);
            prefs::set_nickname(nickname).await.and_then(ok)
        }
        "set_avatar" => {
            let source = param_opt_str(&params, "source_path").map(str::to_owned);
            prefs::set_avatar(source).await.and_then(ok)
        }
        "update_desktop_prefs" => {
            let theme = param_opt_str(&params, "theme").map(str::to_owned);
            let font_size = param_opt(&params, "font_size")
                .and_then(Value::as_u64)
                .and_then(|v| u16::try_from(v).ok());
            let conversation_font = param_opt_str(&params, "conversation_font").map(str::to_owned);
            let reduce_motion = param_opt(&params, "reduce_motion").and_then(Value::as_bool);
            let inspector_open = param_opt(&params, "inspector_open").and_then(Value::as_bool);
            prefs::update_desktop_prefs(
                theme,
                font_size,
                conversation_font,
                reduce_motion,
                inspector_open,
            )
            .await
            .and_then(ok)
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
        "fetch_attachment" => {
            let r = async {
                let method_str = param_string(&params, "method")?.to_owned();
                let uri = param_string(&params, "uri")?.to_owned();
                let scope = Arc::clone(&service.attachment_scope);
                let drafts = Arc::clone(&service.staged_drafts);
                tokio::task::spawn_blocking(move || {
                    let payload = attachment::resolve_attachment_request(
                        scope.as_ref(),
                        &drafts,
                        &method_str,
                        &uri,
                    );
                    use base64::{Engine, engine::general_purpose::STANDARD};
                    soyo_ipc::types::AttachmentPayloadDto {
                        status: payload.status,
                        content_type: payload.content_type.to_owned(),
                        body_b64: STANDARD.encode(&payload.body),
                    }
                })
                .await
                .map_err(|_| DesktopError::new("io_error", "Attachment fetch worker failed"))
            };
            r.await.and_then(ok)
        }
        "submit_prompt" | "compact_session" | "run_prompt" => {
            dispatch_streaming(owner, &method, params, &service, &writer).await
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
    owner: ClientOwner,
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
                id: owner.request_id,
                data: serde_json::to_value(&env).unwrap_or(Value::Null),
            };
            send_frame(&writer_for_events, &frame).await;
        }
    });
    let sink = StreamSink::Mpsc(events_tx);
    let result = match method {
        "submit_prompt" => {
            let prompt_params: soyo_ipc::types::SubmitPromptParams = serde_json::from_value(params)
                .map_err(|err| {
                    DesktopError::new("invalid_params", format!("submit_prompt params: {err}"))
                })?;
            service.submit_prompt_for(owner, prompt_params, sink).await
        }
        "run_prompt" => {
            let run_params: soyo_ipc::types::RunPromptParams = serde_json::from_value(params)
                .map_err(|err| {
                    DesktopError::new("invalid_params", format!("run_prompt params: {err}"))
                })?;
            service.run_prompt_for(owner, run_params, sink).await
        }
        _ => {
            let raw_events = param_bool(&params, "raw_events").unwrap_or(false);
            let sid = param_opt_str(&params, "session_id");
            service
                .compact_session_for(owner, sid, raw_events, sink)
                .await
        }
    };
    let _ = forwarder.await;
    result.and_then(ok)
}
