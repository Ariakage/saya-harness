#[path = "../../saya-core/tests/fixtures/http_support/mod.rs"]
mod fixture;
use serde_json::json;
use std::{
    fs,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::process::CommandExt,
    },
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use tempfile::TempDir;

fn config(root: &Path, url: &str, permissions: &str, default_profile: bool) -> std::path::PathBuf {
    let file = root.join("config.toml");
    fs::write(&file,format!("schema_version = 1\n[defaults]\n{}permissions = \"{permissions}\"\n[profiles.example]\nprotocol = \"openai-chat-completions\"\nbase_url = \"{url}\"\nmodel = \"fake\"\nauth = \"none\"\ncontext_window_tokens = 128000\nstream = false\n",if default_profile{"profile = \"example\"\n"}else{""})).unwrap();
    file
}
fn answer(text: &str) -> String {
    json!({"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":text}}]}).to_string()
}
fn read_call() -> String {
    json!({"choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"tool_calls":[{"id":"read","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"input.txt\",\"start_line\":1,\"line_count\":2}"}}]}}]}).to_string()
}
struct Pty {
    master: Option<fs::File>,
    child: Child,
    buffer: Vec<u8>,
    _storage: TempDir,
}
impl Pty {
    fn start(root: &Path, cfg: &Path, extra: &[&str]) -> Self {
        let storage = TempDir::new().unwrap();
        let mut master = -1;
        let mut slave = -1;
        // SAFETY: arguments point to valid descriptors; ownership moves to File below.
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
        let master = unsafe { fs::File::from_raw_fd(master) };
        let slave = unsafe { fs::File::from_raw_fd(slave) };
        let fd = slave.as_raw_fd();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_saya"));
        cmd.env_clear()
            .env("HOME", root)
            .env("XDG_DATA_HOME", storage.path().join("data"))
            .env("XDG_STATE_HOME", storage.path().join("state"))
            .env("XDG_CACHE_HOME", storage.path().join("cache"))
            .env("PATH", "/usr/bin:/bin")
            .current_dir(root)
            .arg("--config")
            .arg(cfg)
            .args(extra)
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()));
        // SAFETY: pre-exec uses only async-signal-safe syscalls on an inherited descriptor.
        unsafe {
            cmd.pre_exec(move || {
                if libc::setsid() < 0 || libc::ioctl(fd, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().unwrap();
        drop(slave);
        Self {
            master: Some(master),
            child,
            buffer: vec![],
            _storage: storage,
        }
    }
    fn send(&mut self, line: &str) {
        self.master
            .as_mut()
            .unwrap()
            .write_all(line.as_bytes())
            .unwrap();
    }
    fn until(&mut self, needle: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(pos) = self
                .buffer
                .windows(needle.len())
                .position(|w| w == needle.as_bytes())
            {
                return String::from_utf8_lossy(
                    &self.buffer.drain(..pos + needle.len()).collect::<Vec<_>>(),
                )
                .into_owned();
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {needle:?}; output: {}",
                String::from_utf8_lossy(&self.buffer)
            );
            let mut poll = libc::pollfd {
                fd: self.master.as_ref().unwrap().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: poll references the owned PTY descriptor and is time-bounded.
            if unsafe { libc::poll(&mut poll, 1, 100) } <= 0 {
                continue;
            }
            let mut data = [0u8; 4096];
            let n = self.master.as_mut().unwrap().read(&mut data).unwrap_or(0);
            assert!(
                n > 0,
                "terminal closed while waiting for {needle:?}; output: {}",
                String::from_utf8_lossy(&self.buffer)
            );
            self.buffer.extend_from_slice(&data[..n]);
        }
    }
    fn finish(mut self) -> std::process::ExitStatus {
        self.send("/exit\n");
        self.until("Goodbye.");
        drop(self.master.take());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            if Instant::now() >= deadline {
                panic!("chat did not exit");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Pty {
    fn drop(&mut self) {
        drop(self.master.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn continuous_chat_keeps_history_and_changes_mode() {
    let temp = TempDir::new().unwrap();
    let (url, requests) = fixture::server(vec![
        ("application/json", answer("first answer")),
        ("application/json", answer("second answer")),
    ])
    .unwrap();
    let cfg = config(temp.path(), &url, "auto", true);
    let mut pty = Pty::start(temp.path(), &cfg, &["chat"]);
    pty.until("> ");
    pty.send("first request\n");
    pty.until("first answer");
    pty.until("> ");
    pty.send("/mode plan\n");
    pty.until("Mode: plan");
    pty.until("> ");
    pty.send("second request\n");
    pty.until("second answer");
    pty.until("> ");
    assert!(pty.finish().success());
    let _ = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    let second = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    let request = second.to_string();
    assert!(request.contains("first request"));
    assert!(request.contains("first answer"));
    assert!(request.contains("second request"));
    assert!(
        second["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("# Work mode: PLAN")
    );
}
#[test]
fn profile_selection_and_slash_escape_are_handled() {
    let temp = TempDir::new().unwrap();
    let (url, requests) = fixture::server(vec![("application/json", answer("ok"))]).unwrap();
    let cfg = config(temp.path(), &url, "auto", false);
    let mut pty = Pty::start(temp.path(), &cfg, &[]);
    pty.until("Profile (number or name; /exit to quit): ");
    pty.send("1\n");
    pty.until("> ");
    pty.send("//ordinary text\n");
    pty.until("ok");
    pty.until("> ");
    assert!(pty.finish().success());
    assert!(
        requests
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .to_string()
            .contains("/ordinary text")
    );
}
#[test]
fn manual_approval_keeps_permission_setting_immutable_while_running() {
    let temp = TempDir::new().unwrap();
    fs::write(temp.path().join("input.txt"), "data").unwrap();
    let (url, _) = fixture::server(vec![
        ("application/json", read_call()),
        ("application/json", answer("done")),
    ])
    .unwrap();
    let cfg = config(temp.path(), &url, "manual", true);
    let mut pty = Pty::start(temp.path(), &cfg, &["chat"]);
    pty.until("> ");
    pty.send("read it\n");
    pty.until("Allow this call? [y/N] ");
    pty.send("/permissions bypass\n");
    pty.until("Permissions can only change while idle.");
    pty.until("Allow this call? [y/N] ");
    pty.send("y\n");
    pty.until("done");
    pty.until("> ");
    pty.send("/status\n");
    pty.until("permissions: manual");
    pty.until("> ");
    assert!(pty.finish().success());
}
#[test]
fn changing_mode_invalidates_pending_approval() {
    let temp = TempDir::new().unwrap();
    fs::write(temp.path().join("input.txt"), "data").unwrap();
    let (url, requests) = fixture::server(vec![
        ("application/json", read_call()),
        ("application/json", answer("replanned")),
    ])
    .unwrap();
    let cfg = config(temp.path(), &url, "manual", true);
    let mut pty = Pty::start(temp.path(), &cfg, &["chat"]);
    pty.until("> ");
    pty.send("inspect\n");
    pty.until("Allow this call? [y/N] ");
    pty.send("/mode plan\n");
    pty.until("Mode: plan");
    pty.until("replanned");
    pty.until("> ");
    assert!(pty.finish().success());
    let _ = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    let second = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(second.to_string().contains("stale_approval"));
    assert!(
        second["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("# Work mode: PLAN")
    );
}

#[test]
fn chat_clarification_accepts_an_answer_and_returns_it_to_the_model() {
    let temp = TempDir::new().unwrap();
    let args = json!({"question":"Which option?","options":["one","two"]}).to_string();
    let ask=json!({"choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"tool_calls":[{"id":"ask","type":"function","function":{"name":"ask_user","arguments":args}}]}}]}).to_string();
    let (url, requests) = fixture::server(vec![
        ("application/json", ask),
        ("application/json", answer("chosen")),
    ])
    .unwrap();
    let cfg = config(temp.path(), &url, "manual", true);
    let mut pty = Pty::start(temp.path(), &cfg, &["chat"]);
    pty.until("> ");
    pty.send("choose\n");
    pty.until("Allow this call? [y/N] ");
    pty.send("y\n");
    pty.until("Which option?");
    pty.until("Answer: ");
    pty.send("two\n");
    pty.until("chosen");
    pty.until("> ");
    assert!(pty.finish().success());
    let _ = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    let second = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(second.to_string().contains("two"));
}

fn stalled_server() -> (
    String,
    std::sync::mpsc::Receiver<()>,
    std::sync::mpsc::Sender<()>,
) {
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = entered_tx.send(());
            let _ = release_rx.recv_timeout(Duration::from_secs(15));
        }
    });
    (format!("http://{addr}/v1"), entered_rx, release_tx)
}
#[test]
fn running_controls_queue_mode_and_cancel_returns_to_the_same_session() {
    let temp = TempDir::new().unwrap();
    let (url, entered, release) = stalled_server();
    let cfg = config(temp.path(), &url, "auto", true);
    let mut pty = Pty::start(temp.path(), &cfg, &["chat"]);
    pty.until("> ");
    pty.send("long task\n");
    entered.recv_timeout(Duration::from_secs(3)).unwrap();
    pty.send("/mode code\n");
    pty.until("pending code");
    pty.send("/mode plan\n");
    pty.until("pending plan");
    pty.send("/permissions bypass\n");
    pty.until("Permissions can only change while idle.");
    pty.send("/status\n");
    pty.until("pending: plan");
    pty.send("/cancel\n");
    pty.until("Cancelling run...");
    pty.until("> ");
    pty.send("/status\n");
    let status = pty.until("permissions: auto");
    assert!(status.contains("mode: plan"));
    pty.until("> ");
    let _ = release.send(());
    assert!(pty.finish().success());
}
#[test]
fn ctrl_c_cancels_an_active_run_then_exits_when_idle() {
    let temp = TempDir::new().unwrap();
    let (url, entered, release) = stalled_server();
    let cfg = config(temp.path(), &url, "auto", true);
    let mut pty = Pty::start(temp.path(), &cfg, &["chat"]);
    pty.until("> ");
    pty.send("long task\n");
    entered.recv_timeout(Duration::from_secs(3)).unwrap();
    // SAFETY: the child PID identifies the live Saya process started by this test.
    assert_eq!(
        unsafe { libc::kill(pty.child.id() as i32, libc::SIGINT) },
        0
    );
    pty.until("Cancelling run...");
    pty.until("> ");
    assert_eq!(
        unsafe { libc::kill(pty.child.id() as i32, libc::SIGINT) },
        0
    );
    pty.until("Interrupted.");
    drop(pty.master.take());
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = pty.child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "chat did not exit after Ctrl-C");
        std::thread::sleep(Duration::from_millis(10));
    };
    let _ = release.send(());
    assert_eq!(status.code(), Some(130));
}

#[test]
fn cancelling_during_approval_records_tool_as_not_executed() {
    let temp = TempDir::new().unwrap();
    fs::write(temp.path().join("input.txt"), "data").unwrap();
    let (url, requests) = fixture::server(vec![
        ("application/json", read_call()),
        ("application/json", answer("continued")),
    ])
    .unwrap();
    let cfg = config(temp.path(), &url, "manual", true);
    let mut pty = Pty::start(temp.path(), &cfg, &["chat"]);
    pty.until("> ");
    pty.send("inspect\n");
    pty.until("Allow this call? [y/N] ");
    pty.send("/cancel\n");
    pty.until("Cancelling run...");
    pty.until("> ");
    pty.send("continue\n");
    pty.until("continued");
    pty.until("> ");
    assert!(pty.finish().success());
    let _ = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    let second = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(second.to_string().contains("not_executed"));
    assert!(!second.to_string().contains("outcome_unknown"));
}
#[test]
fn idle_permission_change_applies_to_subsequent_runs() {
    let temp = TempDir::new().unwrap();
    fs::write(temp.path().join("input.txt"), "data").unwrap();
    let second_call = read_call().replace("\"id\":\"read\"", "\"id\":\"read-second\"");
    let (url, _) = fixture::server(vec![
        ("application/json", read_call()),
        ("application/json", answer("first done")),
        ("application/json", second_call),
        ("application/json", answer("second done")),
    ])
    .unwrap();
    let cfg = config(temp.path(), &url, "auto", true);
    let mut pty = Pty::start(temp.path(), &cfg, &["chat"]);
    pty.until("> ");
    pty.send("/permissions manual\n");
    pty.until("Permissions: manual");
    pty.until("> ");
    pty.send("first\n");
    pty.until("Allow this call? [y/N] ");
    pty.send("y\n");
    pty.until("first done");
    pty.until("> ");
    pty.send("/permissions bypass\n");
    pty.until("Permissions: bypass");
    pty.until("> ");
    pty.send("second\n");
    let output = pty.until("second done");
    assert!(!output.contains("Allow this call?"));
    pty.until("> ");
    assert!(pty.finish().success());
}

#[test]
fn help_and_control_commands_report_errors_without_ending_session() {
    let temp = TempDir::new().unwrap();
    let cfg = config(temp.path(), "http://127.0.0.1:1/v1", "auto", true);
    let mut pty = Pty::start(temp.path(), &cfg, &["chat"]);
    pty.until("> ");
    pty.send("/help\n");
    let help = pty.until("Use //");
    assert!(help.contains("/mode"));
    assert!(help.contains("/cancel"));
    pty.until("> ");
    pty.send("/exit extra\n");
    pty.until("Invalid command arguments.");
    pty.until("> ");
    pty.send("/unknown\n");
    pty.until("Unknown command");
    pty.until("> ");
    pty.send("/cancel\n");
    pty.until("No active run.");
    pty.until("> ");
    pty.send("/mode\n");
    pty.until("Mode: work");
    pty.until("> ");
    assert!(pty.finish().success());
}

#[test]
fn cancelling_while_awaiting_clarification_records_interrupted_input() {
    let temp = TempDir::new().unwrap();
    let args = json!({"question":"Which option?"}).to_string();
    let ask=json!({"choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"tool_calls":[{"id":"ask","type":"function","function":{"name":"ask_user","arguments":args}}]}}]}).to_string();
    let (url, requests) = fixture::server(vec![
        ("application/json", ask),
        ("application/json", answer("continued")),
    ])
    .unwrap();
    let cfg = config(temp.path(), &url, "auto", true);
    let mut pty = Pty::start(temp.path(), &cfg, &["chat"]);
    pty.until("> ");
    pty.send("choose\n");
    pty.until("Answer: ");
    pty.send("/cancel\n");
    pty.until("Cancelling run...");
    pty.until("> ");
    pty.send("continue\n");
    pty.until("continued");
    pty.until("> ");
    assert!(pty.finish().success());
    let _ = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    let second = requests
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .to_string();
    assert!(second.contains("input_interrupted"));
    assert!(!second.contains("outcome_unknown"));
}
