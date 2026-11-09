#[path = "../../saya-core/tests/fixtures/http_support/mod.rs"]
mod fixture;

use serde_json::json;
use std::{
    ffi::CString,
    fs,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::OpenOptionsExt, process::CommandExt},
    },
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

// The shell and its sleep child both hold the lease. PIDs are namespace-local
// startup metadata, while the FIFO witnesses their lifetime from the host.
const BACKGROUND_PROCESS_SCRIPT: &str = r#"exec 9>tool.lease
printf 'READY\n' >&9
sleep 30 &
printf '%s\n' "$$" > tool.pid.tmp && /bin/mv tool.pid.tmp tool.pid
wait"#;

struct ProcessLease {
    reader: fs::File,
    marker_bytes: usize,
    observed_live: bool,
}
impl ProcessLease {
    const READY: &[u8] = b"READY\n";

    fn new(path: &Path) -> Self {
        let encoded = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: encoded is a valid terminated path in this fixture's private directory.
        assert_eq!(unsafe { libc::mkfifo(encoded.as_ptr(), 0o600) }, 0);
        let reader = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)
            .unwrap();
        Self {
            reader,
            marker_bytes: 0,
            observed_live: false,
        }
    }

    fn live(&mut self) -> bool {
        let mut bytes = [0; 64];
        loop {
            match self.reader.read(&mut bytes) {
                Ok(0) => return false,
                Ok(count) => {
                    let end = self.marker_bytes + count;
                    assert!(end <= Self::READY.len(), "unexpected lease data");
                    assert_eq!(&bytes[..count], &Self::READY[self.marker_bytes..end]);
                    self.marker_bytes = end;
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    // EOF can also contain an old READY marker. Only EAGAIN proves
                    // that a writer still holds the FIFO at this observation.
                    self.observed_live |= self.marker_bytes == Self::READY.len();
                    return self.observed_live;
                }
                Err(error) => panic!("cannot observe process lease: {error}"),
            }
        }
    }

    fn assert_released(&mut self) {
        assert!(self.observed_live, "missing live process positive control");
        assert!(!self.live(), "background process retained its FIFO writer");
    }
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !condition() {
        assert!(Instant::now() < deadline, "fixture timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn pty() -> (fs::File, fs::File) {
    let (mut master, mut slave) = (-1, -1);
    // SAFETY: output pointers are valid and each new descriptor is owned once.
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    // SAFETY: fcntl updates the new owned master descriptor.
    assert_eq!(
        unsafe { libc::fcntl(master, libc::F_SETFL, libc::O_NONBLOCK) },
        0
    );
    // SAFETY: the child must inherit its slave only, never keep the master alive.
    assert_eq!(
        unsafe { libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC) },
        0
    );
    unsafe { (fs::File::from_raw_fd(master), fs::File::from_raw_fd(slave)) }
}
fn prompt(master: &mut fs::File) {
    let mut text = Vec::new();
    while !text.ends_with(b"> ") {
        let mut poll = libc::pollfd {
            fd: master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: the owned descriptor remains live during this bounded poll.
        assert!(unsafe { libc::poll(&mut poll, 1, 5000) } > 0);
        let mut bytes = [0; 1024];
        let count = master.read(&mut bytes).unwrap();
        assert!(count > 0);
        text.extend_from_slice(&bytes[..count]);
    }
}
fn cancellation_case(chat: bool, signal: Option<i32>) {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let mut lease = ProcessLease::new(&workspace.join("tool.lease"));
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (url, _) = fixture::server_dynamic(2, move |index, _| {
        let message = if index == 0 {
            json!({"role":"assistant","content":null,"tool_calls":[{"id":"launch","type":"function","function":{"name":"background_start","arguments":json!({"tool":"exec","arguments":{"argv":["/bin/sh","-c",BACKGROUND_PROCESS_SCRIPT]}}).to_string()}}]})
        } else {
            let _ = release_rx.recv_timeout(Duration::from_secs(10));
            json!({"role":"assistant","content":"done"})
        };
        ("application/json", json!({"choices":[{"index":0,"finish_reason":if index==0 {"tool_calls"} else {"stop"},"message":message}]}).to_string())
    }).unwrap();
    let config = root.path().join("config.toml");
    fs::write(&config, format!("schema_version=1\n[defaults]\nprofile='test'\npermissions='bypass'\n[profiles.test]\nprotocol='openai-chat-completions'\nbase_url='{url}'\nmodel='fixture'\nauth='none'\ncontext_window_tokens=128000\nstream=false\n")).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_saya"));
    command
        .env_clear()
        .env("HOME", root.path())
        .env("PATH", "/usr/bin:/bin")
        .arg("--config")
        .arg(&config)
        .arg("--cwd")
        .arg(&workspace)
        .current_dir(root.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut terminal = None;
    if chat {
        let (master, slave) = pty();
        let fd = slave.as_raw_fd();
        command
            .arg("chat")
            .stdin(Stdio::from(slave.try_clone().unwrap()));
        // SAFETY: only async-signal-safe operations execute before exec.
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() < 0 || libc::ioctl(fd, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        terminal = Some((master, slave));
    } else {
        command.args(["run", "start fixture"]).stdin(Stdio::null());
    }
    let mut child = ChildGuard(command.spawn().unwrap());
    drop(command);
    let mut master = terminal.map(|(master, slave)| {
        drop(slave);
        master
    });
    if let Some(master) = master.as_mut() {
        prompt(master);
        master.write_all(b"start fixture\n").unwrap();
    }
    let pid_file = workspace.join("tool.pid");
    wait_for(|| pid_file.exists());
    let namespace_pid: u32 = fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(namespace_pid > 0);
    wait_for(|| lease.live());
    if let Some(signal) = signal {
        // SAFETY: Child::id is the host PID of the CLI spawned by this fixture.
        assert_eq!(unsafe { libc::kill(child.0.id() as i32, signal) }, 0);
    } else {
        drop(master.take());
    }
    let mut status = None;
    let mut transcript = Vec::new();
    wait_for(|| {
        if let Some(master) = master.as_mut().filter(|_| signal.is_some()) {
            let mut poll = libc::pollfd {
                fd: master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: only the owned live descriptor is observed.
            if unsafe { libc::poll(&mut poll, 1, 0) } > 0 {
                let mut bytes = [0; 4096];
                if let Ok(count) = master.read(&mut bytes) {
                    transcript.extend_from_slice(&bytes[..count]);
                }
            }
        }
        status = child.0.try_wait().unwrap();
        status.is_some()
    });
    let _ = release_tx.send(());
    assert_eq!(
        status.unwrap().code(),
        Some(if signal.is_some() { 130 } else { 2 }),
        "chat={chat} signal={signal:?} tty={:?}",
        String::from_utf8_lossy(&transcript)
    );
    lease.assert_released();
}

#[test]
fn run_termination_and_hangup_await_background_process_cleanup() {
    for signal in [libc::SIGTERM, libc::SIGHUP] {
        cancellation_case(false, Some(signal));
    }
}

#[test]
fn chat_termination_and_hangup_await_background_process_cleanup() {
    for signal in [libc::SIGTERM, libc::SIGHUP] {
        cancellation_case(true, Some(signal));
    }
}

#[test]
fn chat_terminal_loss_awaits_background_process_cleanup() {
    cancellation_case(true, None);
}

#[test]
fn broken_stream_stdout_returns_an_error_after_background_cleanup() {
    broken_stdout_case(false);
}
#[test]
fn completed_only_stdout_error_waits_for_background_cleanup() {
    broken_stdout_case(true);
}
fn broken_stdout_case(completed_only: bool) {
    let root = tempfile::tempdir().unwrap();
    let mut lease = ProcessLease::new(&root.path().join("tool.lease"));
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (url, _) = fixture::server_dynamic(2, move |index, _| {
        if completed_only {
            let item = if index == 0 {
                json!({"type":"function_call","id":"launch-item","call_id":"launch","status":"completed",
                    "name":"background_start","arguments":json!({"tool":"exec","arguments":{"argv":["/bin/sh","-c",BACKGROUND_PROCESS_SCRIPT]}}).to_string()})
            } else {
                let _ = release_rx.recv_timeout(Duration::from_secs(10));
                json!({"type":"message","id":"reply","role":"assistant","status":"completed",
                    "content":[{"type":"output_text","text":"fixture text\n".repeat(1024)}]})
            };
            return ("text/event-stream", fixture::sse(&[json!({"type":"response.completed","response":{"status":"completed","output":[item]}})]));
        }
        if index == 0 {
            let call = json!({"id":"launch","type":"function","function":{"name":"background_start","arguments":json!({"tool":"exec","arguments":{"argv":["/bin/sh","-c",BACKGROUND_PROCESS_SCRIPT]}}).to_string()}});
            return ("text/event-stream", fixture::sse(&[
                json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":call["id"],"type":"function","function":call["function"]}]},"finish_reason":null}]}),
                json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
            ]));
        }
        let _ = release_rx.recv_timeout(Duration::from_secs(10));
        ("text/event-stream", fixture::sse(&[
            json!({"choices":[{"index":0,"delta":{"content":"fixture text\n".repeat(1024)},"finish_reason":null}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
        ]))
    }).unwrap();
    let config = root.path().join("config.toml");
    let protocol = if completed_only {
        "openai-responses"
    } else {
        "openai-chat-completions"
    };
    fs::write(&config, format!("schema_version=1\n[defaults]\nprofile='test'\npermissions='bypass'\n[profiles.test]\nprotocol='{protocol}'\nbase_url='{url}'\nmodel='fixture'\nauth='none'\ncontext_window_tokens=128000\nstream=true\n")).unwrap();
    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_saya"))
            .env_clear()
            .env("HOME", root.path())
            .env("PATH", "/usr/bin:/bin")
            .current_dir(root.path())
            .arg("--config")
            .arg(config)
            .args(["run", "fixture"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let marker = root.path().join("tool.pid");
    wait_for(|| marker.exists());
    let namespace_pid = fs::read_to_string(marker)
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();
    assert!(namespace_pid > 0);
    wait_for(|| lease.live());
    drop(child.0.stdout.take());
    release_tx.send(()).unwrap();
    let mut status = None;
    wait_for(|| {
        status = child.0.try_wait().unwrap();
        status.is_some()
    });
    lease.assert_released();
    assert_eq!(status.unwrap().code(), Some(1));
    let mut stderr = String::new();
    child
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(!stderr.contains("panicked"), "{stderr}");
}

#[test]
fn process_lease_requires_live_control_and_waits_for_inherited_writers() {
    let root = tempfile::tempdir().unwrap();
    let mut historical = ProcessLease::new(&root.path().join("historical.lease"));
    assert!(!historical.live());
    assert!(!historical.observed_live);
    let mut writer = fs::OpenOptions::new()
        .write(true)
        .open(root.path().join("historical.lease"))
        .unwrap();
    writer.write_all(ProcessLease::READY).unwrap();
    drop(writer);
    assert!(!historical.live());
    assert!(!historical.observed_live, "historical READY is not live");

    let mut inherited = ProcessLease::new(&root.path().join("tool.lease"));
    let mut parent = ChildGuard(
        Command::new("/bin/sh")
            .args([
                "-c",
                "exec 8<&0; exec 9>tool.lease; printf 'READY\\n' >&9; /bin/sh -c 'IFS= read -r release <&8' &",
            ])
            .current_dir(root.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut release = parent.0.stdin.take().unwrap();
    assert!(parent.0.wait().unwrap().success());
    wait_for(|| inherited.live());
    assert!(
        inherited.live(),
        "descendant must retain its inherited writer"
    );
    release.write_all(b"release\n").unwrap();
    wait_for(|| !inherited.live());
    inherited.assert_released();
}
