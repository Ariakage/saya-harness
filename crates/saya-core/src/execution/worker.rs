//! Private, single-request file RPC and the Linux payload launcher.
//! The CLI dispatches these before configuration, logging or project discovery.
use super::failure;
use crate::{
    boundary::{Cancellation, Store, StoreChange, Tool, ToolContext},
    contract::{BoundaryError, ErrorInfo, ModelProtocol, Session, ToolCall, ToolResult},
    memory::MemoryStore,
    tools::{
        read::{ProtectedPathSnapshot, ReadGate, ReadLimits, ReadTool, ReadToolKind, ReadTools},
        write::{WriteGate, WriteTool, WriteToolKind},
    },
};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

pub const WORKER_RPC_MAX_BYTES: usize = 32 * 1024 * 1024;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileWorkerRequest {
    pub schema_version: u8,
    pub call: ToolCall,
    pub context: ToolContext,
    pub approved_target: PathBuf,
    pub read_roots: Vec<PathBuf>,
    pub read_limits: ReadLimits,
    pub write_max_bytes: usize,
    #[serde(default)]
    pub protected_paths: ProtectedPathSnapshot,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileWorkerResponse {
    pub schema_version: u8,
    pub result: ToolResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundary_error: Option<ErrorInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_result: Option<ToolResult>,
}
/// Recognize private commands using arguments without argv[0]. Returns their
/// process exit code. Call before any app setup or subscriber initialization.
pub async fn internal_command(args: Vec<OsString>) -> Option<i32> {
    match args.first().and_then(|value| value.to_str()) {
        Some("__saya-file-worker") => {
            if args.len() != 1 {
                return Some(64);
            }
            Some(file_worker_stdio().await)
        }
        Some("__saya-sandbox-launch") => Some(launch::run(&args[1..])),
        _ => None,
    }
}
/// Execute only the one host-approved file call, without configuration or logs.
/// The parent owns durable intent, approval and final result persistence.
pub async fn execute_file_worker(request: FileWorkerRequest) -> Result<ToolResult, BoundaryError> {
    validate_request(&request)?;
    let files = Arc::new(
        ReadTools::new(
            &request.context.workspace,
            &request.read_roots,
            false,
            request.read_limits.clone(),
        )
        .map_err(|error| failure(error.code, error.message))?,
    );
    let gate = Arc::new(BoundGate {
        call: request.call.clone(),
        context: request.context.clone(),
        target: request.approved_target.clone(),
    });
    files
        .install_protection_snapshot(request.protected_paths.clone())
        .map_err(|error| failure(error.code, error.message))?;
    let store: Arc<dyn Store> = Arc::new(MemoryStore::default());
    store
        .commit(&[
            StoreChange::Session(Session {
                id: request.context.run.session_id,
                workspace: request.context.workspace.clone(),
                work_mode: request.context.run.work_mode,
                profile: "internal-file-worker".into(),
                protocol: ModelProtocol::ChatCompletions,
                model: "none".into(),
                history: Vec::new(),
            }),
            StoreChange::RunStarted(request.context.run.clone()),
        ])
        .await?;
    let tool: Box<dyn Tool> = match request.call.name.as_str() {
        "read_file" => Box::new(ReadTool::with_gate(files, ReadToolKind::ReadFile, gate)),
        "list_dir" => Box::new(ReadTool::with_gate(files, ReadToolKind::ListDir, gate)),
        "find_files" => Box::new(ReadTool::with_gate(files, ReadToolKind::FindFiles, gate)),
        "search_text" => Box::new(ReadTool::with_gate(files, ReadToolKind::SearchText, gate)),
        "write_file" => Box::new(WriteTool::new(
            files,
            store,
            gate,
            WriteToolKind::WriteFile,
            request.write_max_bytes,
        )),
        "apply_patch" => Box::new(WriteTool::new(
            files,
            store,
            gate,
            WriteToolKind::ApplyPatch,
            request.write_max_bytes,
        )),
        _ => {
            return Err(failure(
                "unsupported_worker_tool",
                "file workers only accept the six local file tools",
            ));
        }
    };
    tool.execute(&request.call, &request.context, &NeverCancelled)
        .await
}
fn validate_request(request: &FileWorkerRequest) -> Result<(), BoundaryError> {
    request
        .protected_paths
        .validate()
        .map_err(|error| failure(error.code, error.message))?;
    if request.schema_version != 1 {
        return Err(failure(
            "unsupported_worker_schema",
            "file worker schema must be 1",
        ));
    }
    if !request.approved_target.is_absolute() || request.read_roots.len() > 128 {
        return Err(failure(
            "invalid_worker_request",
            "worker requires an absolute approved target and at most 128 roots",
        ));
    }
    let limits = &request.read_limits;
    for value in [
        limits.read_return_bytes,
        limits.search_bytes,
        limits.list_bytes,
        request.write_max_bytes,
    ] {
        if value == 0 {
            return Err(failure(
                "invalid_worker_request",
                "worker byte limits must be positive",
            ));
        }
    }
    if limits.file_bytes == 0 {
        return Err(failure(
            "invalid_worker_request",
            "worker file scan limit must be positive",
        ));
    }
    if limits.search_results == 0
        || limits.search_results > 100_000
        || limits.list_entries == 0
        || limits.list_entries > 100_000
    {
        return Err(failure(
            "invalid_worker_request",
            "worker item limits must be 1..100000",
        ));
    }
    Ok(())
}
struct BoundGate {
    call: ToolCall,
    context: ToolContext,
    target: PathBuf,
}
impl BoundGate {
    fn allows(&self, call: &ToolCall, context: &ToolContext, target: &Path) -> bool {
        call == &self.call
            && context.run == self.context.run
            && context.workspace == self.context.workspace
            && target == self.target
    }
}
impl ReadGate for BoundGate {
    fn authorize(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        target: &Path,
    ) -> Result<bool, BoundaryError> {
        Ok(self.allows(call, context, target))
    }
}
impl WriteGate for BoundGate {
    fn authorize(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        target: &Path,
    ) -> Result<bool, BoundaryError> {
        Ok(self.allows(call, context, target))
    }
}
struct NeverCancelled;
impl Cancellation for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}
async fn file_worker_stdio() -> i32 {
    let mut input = Vec::new();
    let read = {
        let stdin = io::stdin();
        let mut reader = stdin.lock().take(WORKER_RPC_MAX_BYTES as u64 + 1);
        reader.read_to_end(&mut input)
    };
    let response = match read {
        Ok(_) if input.len() <= WORKER_RPC_MAX_BYTES => {
            match serde_json::from_slice::<FileWorkerRequest>(&input) {
                Ok(request) => match execute_file_worker(request).await {
                    Ok(result) => FileWorkerResponse {
                        schema_version: 1,
                        result,
                        boundary_error: None,
                        observed_result: None,
                    },
                    Err(error) => FileWorkerResponse {
                        schema_version: 1,
                        result: failed(error.0.clone()),
                        boundary_error: Some(error.0),
                        observed_result: error.2.map(|value| *value),
                    },
                },
                Err(_) => protocol_failure(
                    "invalid_worker_request",
                    "worker input must contain one valid request object",
                ),
            }
        }
        Ok(_) => protocol_failure("worker_rpc_too_large", "worker input exceeds 32 MiB"),
        Err(_) => protocol_failure("worker_io_error", "worker input could not be read"),
    };
    let mut output = BoundedBuffer(Vec::new());
    if serde_json::to_writer(&mut output, &response).is_err() {
        output.0.clear();
        if serde_json::to_writer(
            &mut output,
            &protocol_failure("worker_rpc_too_large", "worker response exceeds 32 MiB"),
        )
        .is_err()
        {
            return 74;
        }
    }
    let mut stdout = io::stdout().lock();
    if stdout
        .write_all(&output.0)
        .and_then(|_| stdout.write_all(b"\n"))
        .and_then(|_| stdout.flush())
        .is_err()
    {
        return 74;
    }
    0
}
fn failed(error: ErrorInfo) -> ToolResult {
    ToolResult {
        ok: false,
        data: None,
        error: Some(error),
        truncated: false,
    }
}
fn protocol_failure(code: &str, message: &str) -> FileWorkerResponse {
    FileWorkerResponse {
        schema_version: 1,
        result: failed(ErrorInfo {
            code: code.into(),
            message: message.into(),
        }),
        boundary_error: None,
        observed_result: None,
    }
}
struct BoundedBuffer(Vec<u8>);
impl Write for BoundedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > (WORKER_RPC_MAX_BYTES - 1).saturating_sub(self.0.len()) {
            return Err(io::Error::from_raw_os_error(libc::EFBIG));
        }
        self.0
            .try_reserve(bytes.len())
            .map_err(|_| io::Error::from_raw_os_error(libc::ENOMEM))?;
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

mod launch {
    use super::*;
    #[cfg(target_os = "linux")]
    use std::{
        net::{SocketAddr, TcpStream},
        os::unix::fs::MetadataExt,
        process::{Child, Stdio},
        time::{Duration, Instant},
    };
    use std::{
        os::unix::process::{CommandExt, ExitStatusExt},
        process::Command,
    };
    struct Launch {
        socat: Option<PathBuf>,
        socat_identity: Option<(u64, u64)>,
        socket: Option<PathBuf>,
        port: Option<u16>,
        env_fd: i32,
        argv: Vec<OsString>,
    }
    fn parse(args: &[OsString]) -> io::Result<Launch> {
        let mut socat = None;
        let mut socat_identity = None;
        let mut socket = None;
        let mut port = None;
        let mut env_fd = None;
        let mut argv = Vec::new();
        let invalid = || io::Error::from_raw_os_error(libc::EINVAL);
        let mut index = 0;
        while index < args.len() {
            if args[index] == "--" {
                argv = args[index + 1..].to_vec();
                break;
            }
            let value = args.get(index + 1).ok_or_else(invalid)?;
            match args[index].to_str() {
                Some("--socat") if socat.is_none() => socat = Some(PathBuf::from(value)),
                Some("--socat-identity") if socat_identity.is_none() => {
                    let (device, inode) = value
                        .to_str()
                        .and_then(|value| value.split_once(':'))
                        .ok_or_else(invalid)?;
                    socat_identity = Some((
                        device.parse().map_err(|_| invalid())?,
                        inode.parse().map_err(|_| invalid())?,
                    ));
                }
                Some("--socket") if socket.is_none() => socket = Some(PathBuf::from(value)),
                Some("--port") if port.is_none() => {
                    port = Some(
                        value
                            .to_str()
                            .and_then(|value| value.parse::<u16>().ok())
                            .filter(|port| *port != 0)
                            .ok_or_else(invalid)?,
                    );
                }
                Some("--env-fd") if env_fd.is_none() => {
                    env_fd = Some(
                        value
                            .to_str()
                            .and_then(|value| value.parse::<i32>().ok())
                            .filter(|fd| *fd >= 3)
                            .ok_or_else(invalid)?,
                    );
                }
                _ => return Err(invalid()),
            }
            index += 2;
        }
        if argv.is_empty() || !Path::new(&argv[0]).is_absolute() {
            return Err(invalid());
        }
        let fields = [
            socat.is_some(),
            socat_identity.is_some(),
            socket.is_some(),
            port.is_some(),
        ];
        if fields.iter().any(|value| *value) && !fields.iter().all(|value| *value) {
            return Err(invalid());
        }
        #[cfg(target_os = "linux")]
        if let Some(socket) = &socket
            && !super::super::linux::safe_socat_path(socket)
        {
            return Err(invalid());
        }
        #[cfg(target_os = "macos")]
        if fields.iter().any(|value| *value) {
            return Err(invalid());
        }
        Ok(Launch {
            socat,
            socat_identity,
            socket,
            port,
            env_fd: env_fd.ok_or_else(invalid)?,
            argv,
        })
    }
    pub(super) fn run(args: &[OsString]) -> i32 {
        // Bubblewrap's namespace init is PID 1 and its initial command is PID 2.
        #[cfg(target_os = "linux")]
        if std::process::id() != 2 {
            return 69;
        }
        #[cfg(target_os = "linux")]
        let _children = NamespaceChildren;
        let Ok(launch) = parse(args) else {
            return 64;
        };
        match execute(launch) {
            Ok(code) => code,
            Err(error) => {
                eprintln!(
                    "saya sandbox payload failed (os error {})",
                    error.raw_os_error().unwrap_or(libc::EIO)
                );
                74
            }
        }
    }
    fn execute(launch: Launch) -> io::Result<i32> {
        let environment = super::super::transport::read_environment(launch.env_fd)?;
        #[cfg(target_os = "linux")]
        let bridge = start_bridge(&launch)?;
        #[cfg(target_os = "macos")]
        let _ = (
            &launch.socat,
            launch.socat_identity,
            &launch.socket,
            launch.port,
        );
        #[cfg(target_os = "linux")]
        let filter = super::super::prepare_linux_payload_filter()?;
        let mut descriptors = super::super::fds::DescriptorSanitizer::prepare()?;
        let mut payload = Command::new(&launch.argv[0]);
        payload
            .args(&launch.argv[1..])
            .env_clear()
            .envs(environment);
        // SAFETY: storage was prepared before fork. Environment restoration is
        // handled by Command, while the child callback only invokes syscalls.
        unsafe {
            payload.pre_exec(move || {
                descriptors.mark_close_on_exec()?;
                #[cfg(target_os = "linux")]
                filter.install()?;
                Ok(())
            });
        }
        let status = payload.status()?;
        #[cfg(target_os = "linux")]
        drop(bridge);
        Ok(status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)))
    }
    #[cfg(target_os = "linux")]
    fn start_bridge(launch: &Launch) -> io::Result<Option<Bridge>> {
        let (Some(socat), Some(identity), Some(socket), Some(port)) = (
            &launch.socat,
            launch.socat_identity,
            &launch.socket,
            launch.port,
        ) else {
            return Ok(None);
        };
        verify_bridge_program(socat, identity)?;
        let socket = socket
            .to_str()
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
        let mut command = Command::new(socat);
        command
            .args([
                format!("TCP4-LISTEN:{port},bind=127.0.0.1,reuseaddr,fork"),
                format!("UNIX-CONNECT:{socket}"),
            ])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let mut descriptors = super::super::fds::DescriptorSanitizer::prepare()?;
        // SAFETY: descriptor storage was allocated before fork.
        unsafe {
            command.pre_exec(move || descriptors.mark_close_on_exec());
        }
        let group = Bridge(command.spawn()?);
        let deadline = Instant::now() + Duration::from_secs(3);
        let endpoint = SocketAddr::from(([127, 0, 0, 1], port));
        loop {
            if TcpStream::connect_timeout(&endpoint, Duration::from_millis(50)).is_ok() {
                return Ok(Some(group));
            }
            if Instant::now() >= deadline {
                return Err(io::Error::from_raw_os_error(libc::ETIMEDOUT));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    #[cfg(target_os = "linux")]
    fn verify_bridge_program(path: &Path, identity: (u64, u64)) -> io::Result<()> {
        let metadata = std::fs::metadata(path)?;
        if !path.is_absolute()
            || !metadata.is_file()
            || (metadata.dev(), metadata.ino()) != identity
            || metadata.mode() & 0o222 != 0
            || metadata.mode() & 0o111 == 0
        {
            return Err(io::Error::from_raw_os_error(libc::EPERM));
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    struct Bridge(Child);
    #[cfg(target_os = "linux")]
    struct NamespaceChildren;
    #[cfg(target_os = "linux")]
    impl Drop for NamespaceChildren {
        fn drop(&mut self) {
            // PID 2 was checked before this guard was created. Linux kill(-1)
            // excludes self and namespace init, and cannot see host processes.
            unsafe {
                libc::kill(-1, libc::SIGKILL);
            }
        }
    }
    #[cfg(target_os = "linux")]
    impl Drop for Bridge {
        fn drop(&mut self) {
            if let Ok(pid) = i32::try_from(self.0.id()) {
                // The leader remains unreaped while its group is signaled.
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
            }
            let _ = self.0.wait();
        }
    }
    #[cfg(all(test, target_os = "linux"))]
    mod tests {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        #[test]
        fn bridge_launch_requires_one_complete_and_valid_identity() {
            let arguments = [
                "--socat",
                "/private/saya-socat",
                "--socat-identity",
                "12:34",
                "--socket",
                "/private/broker.sock",
                "--port",
                "1234",
                "--env-fd",
                "3",
                "--",
                "/usr/bin/true",
            ]
            .map(OsString::from);
            assert_eq!(
                parse(&arguments).expect("complete bridge").socat_identity,
                Some((12, 34))
            );
            for invalid in [
                "",
                "12",
                "12:",
                "x:34",
                "12:34:56",
                "18446744073709551616:34",
            ] {
                let mut malformed = arguments.clone();
                malformed[3] = invalid.into();
                assert!(parse(&malformed).is_err(), "accepted {invalid:?}");
            }
            let mut missing = arguments.to_vec();
            missing.drain(2..4);
            assert!(parse(&missing).is_err());
            let ordinary = ["--env-fd", "3", "--", "/usr/bin/true"].map(OsString::from);
            assert!(
                parse(&ordinary)
                    .expect("ordinary payload")
                    .socat_identity
                    .is_none()
            );
        }

        #[test]
        fn bridge_recheck_requires_the_pinned_readonly_image_identity() {
            let directory = tempfile::tempdir().expect("private bridge directory");
            let path = directory.path().join("saya-socat");
            std::fs::write(&path, b"private image").expect("fixture image");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500))
                .expect("readonly executable");
            let pinned = std::fs::File::open(&path).expect("keep original inode alive");
            let metadata = pinned.metadata().expect("image metadata");
            let identity = (metadata.dev(), metadata.ino());
            verify_bridge_program(&path, identity)
                .expect("host-verified image is identified by inode");
            assert!(
                verify_bridge_program(&path, (identity.0, identity.1.wrapping_add(1))).is_err()
            );
            for mode in [0o700, 0o400] {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
                    .expect("changed image mode");
                assert!(
                    verify_bridge_program(&path, identity).is_err(),
                    "accepted {mode:o}"
                );
            }
            std::fs::remove_file(&path).expect("replace private pathname");
            std::fs::write(&path, b"replacement").expect("replacement image");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500))
                .expect("replacement mode");
            assert!(verify_bridge_program(&path, identity).is_err());
        }
    }
}
