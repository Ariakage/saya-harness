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
    sync::Arc,
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
    _storage: Arc<TempDir>,
}
impl Pty {
    fn start(root: &Path, cfg: &Path, extra: &[&str]) -> Self {
        Self::start_with_storage(root, cfg, extra, Arc::new(TempDir::new().unwrap()))
    }
    fn start_with_storage(root: &Path, cfg: &Path, extra: &[&str], storage: Arc<TempDir>) -> Self {
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
            .env("HOME", storage.path())
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
    fn read_output(&mut self, wait_millis: i32) -> io::Result<bool> {
        let mut poll = libc::pollfd {
            fd: self.master.as_ref().unwrap().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll references the owned PTY descriptor and is time-bounded.
        let ready = unsafe { libc::poll(&mut poll, 1, wait_millis) };
        if ready == 0 {
            return Ok(false);
        }
        if ready < 0 {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::Interrupted {
                Ok(false)
            } else {
                Err(error)
            };
        }
        let mut data = [0u8; 4096];
        let count = self.master.as_mut().unwrap().read(&mut data)?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "terminal closed",
            ));
        }
        self.buffer.extend_from_slice(&data[..count]);
        Ok(true)
    }
    fn wait_for_model_signal(&mut self, signal: &std::sync::mpsc::Receiver<()>) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let status = self.child.try_wait().unwrap();
            if status.is_some() {
                // Capture queued terminal output without waiting for writers.
                for _ in 0..16 {
                    if !self.read_output(0).unwrap_or(false) {
                        break;
                    }
                }
                panic!(
                    "chat exited before model readiness; status: {status:?}; output: {}",
                    String::from_utf8_lossy(&self.buffer)
                );
            }
            match signal.try_recv() {
                Ok(()) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => panic!(
                    "model readiness signal disconnected; child status: {status:?}; output: {}",
                    String::from_utf8_lossy(&self.buffer)
                ),
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for model readiness; child status: {status:?}; output: {}",
                String::from_utf8_lossy(&self.buffer)
            );
            if let Err(error) = self.read_output(100) {
                panic!(
                    "terminal closed before model readiness ({error}); child status: {:?}; output: {}",
                    self.child.try_wait().unwrap(),
                    String::from_utf8_lossy(&self.buffer)
                );
            }
        }
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
            if let Err(error) = self.read_output(100) {
                panic!(
                    "terminal closed while waiting for {needle:?} ({error}); output: {}",
                    String::from_utf8_lossy(&self.buffer)
                );
            }
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
fn model_readiness_reports_early_exit_with_terminal_output() {
    let temp = TempDir::new().unwrap();
    let cfg = config(temp.path(), "http://127.0.0.1:1/v1", "auto", true);
    let mut pty = Pty::start(temp.path(), &cfg, &["--help"]);
    let (_sender, signal) = std::sync::mpsc::channel();
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pty.wait_for_model_signal(&signal);
    }))
    .expect_err("an exited process cannot become model-ready");
    let message = failure.downcast_ref::<String>().unwrap();
    assert!(message.contains("model readiness"), "{message}");
    assert!(message.contains("status:"), "{message}");
    assert!(message.contains("Usage:"), "{message}");
    assert!(!message.contains("timed out"), "{message}");
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
fn resume_waits_for_input_and_uses_original_mode_with_new_permissions() {
    let temp = TempDir::new().unwrap();
    let storage = Arc::new(TempDir::new().unwrap());
    let (url, requests) = fixture::server(vec![
        ("application/json", answer("first answer")),
        ("application/json", answer("second answer")),
    ])
    .unwrap();
    let cfg = config(temp.path(), &url, "auto", true);
    let mut first = Pty::start_with_storage(temp.path(), &cfg, &["chat"], storage.clone());
    first.until("> ");
    first.send("first task\n");
    first.until("first answer");
    first.until("> ");
    first.send("/mode plan\n");
    first.until("Mode: plan");
    first.until("> ");
    assert!(first.finish().success());
    let _ = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    let listed = Command::new(env!("CARGO_BIN_EXE_saya"))
        .env_clear()
        .env("HOME", storage.path())
        .env("XDG_DATA_HOME", storage.path().join("data"))
        .env("XDG_STATE_HOME", storage.path().join("state"))
        .env("XDG_CACHE_HOME", storage.path().join("cache"))
        .current_dir(temp.path())
        .arg("--config")
        .arg(&cfg)
        .args(["--output", "json", "session", "list"])
        .output()
        .unwrap();
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let list: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    let id = list["data"][0]["id"].as_str().unwrap();
    let mut resumed = Pty::start_with_storage(
        temp.path(),
        &cfg,
        &["--permissions", "manual", "resume", id],
        storage.clone(),
    );
    resumed.until("> ");
    assert!(requests.try_recv().is_err());
    resumed.send("/status\n");
    resumed.until("mode: plan; pending: none; permissions: manual");
    resumed.until("> ");
    resumed.send("second task\n");
    resumed.until("second answer");
    resumed.until("> ");
    assert!(resumed.finish().success());
    let request = requests
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .to_string();
    assert!(request.contains("first task"));
    assert!(request.contains("first answer"));
    let mut overridden = Pty::start_with_storage(
        temp.path(),
        &cfg,
        &["--mode", "work", "--permissions", "bypass", "resume", id],
        storage,
    );
    overridden.until("> ");
    assert!(requests.try_recv().is_err());
    overridden.send("/status\n");
    overridden.until("mode: work; pending: none; permissions: bypass");
    overridden.until("> ");
    assert!(overridden.finish().success());
    assert!(request.contains("second task"));
}

#[test]
fn persistence_failure_shows_observed_result_and_ends_chat() {
    let temp = TempDir::new().unwrap();
    let storage = Arc::new(TempDir::new().unwrap());
    fs::write(temp.path().join("input.txt"), "readable fixture\n").unwrap();
    let (url, _) = fixture::server(vec![
        ("application/json", answer("first")),
        ("application/json", read_call()),
    ])
    .unwrap();
    let cfg = config(temp.path(), &url, "auto", true);
    let mut pty = Pty::start_with_storage(temp.path(), &cfg, &["chat"], storage.clone());
    pty.until("> ");
    pty.send("first task\n");
    pty.until("first");
    pty.until("> ");
    let db = rusqlite::Connection::open(storage.path().join("data/saya/sessions.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_tool_result BEFORE UPDATE OF result ON tool_calls BEGIN SELECT RAISE(ABORT,'test failure'); END;").unwrap();
    drop(db);
    pty.send("read task\n");
    pty.until("observed tool result (not saved)");
    pty.until("readable fixture");
    drop(pty.master.take());
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = pty.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "chat continued after a persistence failure"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(1));
}
#[test]
fn profile_selection_and_slash_escape_are_handled() {
    let temp = TempDir::new().unwrap();
    let (url, requests) = fixture::server(vec![("application/json", answer("ok"))]).unwrap();
    let cfg = config(temp.path(), &url, "auto", false);
    let mut pty = Pty::start(temp.path(), &cfg, &["chat"]);
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
    pty.wait_for_model_signal(&entered);
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
    pty.wait_for_model_signal(&entered);
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
    assert!(help.contains("/compact"));
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

fn compact_summary(request: &serde_json::Value) -> String {
    let data: serde_json::Value = serde_json::from_str(
        request["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let mut objective = Vec::new();
    for unit in data["source_units"].as_array().unwrap() {
        for source in unit.as_array().unwrap() {
            if source["role"] == "user" {
                objective.push(json!({"text":"Continue the recorded user task.","source_ids":[source["source_id"]],"evidence":"user_request","status":"active"}));
            }
        }
    }
    answer(&json!({"schema_version":1,"covered_range":data["assigned_source_range"],"objective":objective,"constraints":[],"decisions":[],"completed":[],"remaining":[],"uncertainties":[],"required_ids":[],"references":[]}).to_string())
}
fn saved_history(storage: &Path) -> Vec<String> {
    let db = rusqlite::Connection::open(storage.join("data/saya/sessions.sqlite3")).unwrap();
    let mut statement = db
        .prepare("SELECT item FROM messages ORDER BY session_id,ordinal")
        .unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[test]
fn idle_chat_compact_preserves_raw_history_and_uses_only_one_tool_free_summary_request() {
    let temp = TempDir::new().unwrap();
    let storage = Arc::new(TempDir::new().unwrap());
    let (url, requests) = fixture::server_dynamic(2, |index, request| {
        (
            "application/json",
            if index == 0 {
                answer(&format!(
                    "first answer {}",
                    "Earlier repetitive observation. ".repeat(300)
                ))
            } else {
                compact_summary(request)
            },
        )
    })
    .unwrap();
    let cfg = config(temp.path(), &url, "auto", true);
    let mut pty = Pty::start_with_storage(temp.path(), &cfg, &["chat"], storage.clone());
    pty.until("> ");
    pty.send("first task\n");
    pty.until("first answer");
    pty.until("> ");
    let raw = saved_history(storage.path());
    assert_eq!(raw.len(), 2);
    pty.send("/compact extra\n");
    pty.until("Invalid command arguments.");
    pty.until("> ");
    pty.send("/compact\n");
    pty.until("Context checkpoint published");
    pty.until("> ");
    assert_eq!(saved_history(storage.path()), raw);
    let first = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(first.get("tools").is_some());
    let summary = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(summary.get("tools").is_none());
    assert!(requests.try_recv().is_err());
    assert!(pty.finish().success());
}

#[test]
fn chat_compact_queued_during_the_final_answer_runs_after_the_existing_run() {
    let temp = TempDir::new().unwrap();
    let storage = Arc::new(TempDir::new().unwrap());
    let (entered, ready) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let (url, requests) = fixture::server_dynamic(2, move |index, request| {
        if index == 0 {
            entered.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(5)).unwrap();
            (
                "application/json",
                answer(&format!(
                    "final answer {}",
                    "Earlier repetitive observation. ".repeat(300)
                )),
            )
        } else {
            ("application/json", compact_summary(request))
        }
    })
    .unwrap();
    let cfg = config(temp.path(), &url, "auto", true);
    let mut pty = Pty::start_with_storage(temp.path(), &cfg, &["chat"], storage.clone());
    pty.until("> ");
    pty.send("only task\n");
    pty.wait_for_model_signal(&ready);
    pty.send("/compact\n");
    pty.until("Compaction requested for the next model boundary.");
    release.send(()).unwrap();
    pty.until("Context checkpoint published");
    pty.until("> ");
    assert_eq!(saved_history(storage.path()).len(), 2);
    let first = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(first.get("tools").is_some());
    let second = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(second.get("tools").is_none());
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

#[test]
fn background_job_can_be_approved_listed_and_inspected_in_chat() {
    let temp = TempDir::new().unwrap();
    fs::write(temp.path().join("input.txt"), "background data").unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (url,_)=fixture::server_dynamic(2,move |index,_| {
        if index==0 {
            let arguments=json!({"tool":"read_file","arguments":{"path":"input.txt","start_line":1,"line_count":1}}).to_string();
            ("application/json",json!({"choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"tool_calls":[{"id":"launch","type":"function","function":{"name":"background_start","arguments":arguments}}]}}]}).to_string())
        } else {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            ("application/json",answer("done"))
        }
    }).unwrap();
    let cfg = config(temp.path(), &url, "manual", true);
    let mut pty = Pty::start(temp.path(), &cfg, &["chat"]);
    pty.until("> ");
    pty.send("start background read\n");
    pty.until("Approval required");
    pty.until("Allow this call? [y/N]");
    pty.send("y\n");
    pty.wait_for_model_signal(&entered_rx);
    pty.send("/jobs\n");
    let listed = pty.until("read_file");
    let job = listed
        .split_whitespace()
        .find(|item| item.starts_with("background-"))
        .unwrap()
        .to_owned();
    pty.until("\n");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        pty.send(&format!("/job {job}\n"));
        pty.until("\"status\":\"");
        let status = pty.until("\"");
        if status == "finished\"" {
            break;
        }
        assert_eq!(status, "running\"");
        assert!(Instant::now() < deadline, "background job did not finish");
        std::thread::sleep(Duration::from_millis(10));
    }
    pty.send(&format!("/cancel-job {job}\n"));
    pty.until("Background job already finished.");
    release_tx.send(()).unwrap();
    pty.until("done");
    pty.until("> ");
    assert!(pty.finish().success());
}

#[test]
fn running_background_command_can_be_cancelled_from_chat() {
    let temp = TempDir::new().unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (url, _) = fixture::server_dynamic(2, move |index, _| {
        if index == 0 {
            let arguments = json!({"tool":"exec","arguments":{"argv":["/bin/sh","-c","printf started > marker; sleep 30"],"cwd":null,"timeout_seconds":null}}).to_string();
            ("application/json", json!({"choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"tool_calls":[{"id":"launch","type":"function","function":{"name":"background_start","arguments":arguments}}]}}]}).to_string())
        } else {
            entered_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(10));
            ("application/json", answer("done"))
        }
    }).unwrap();
    let cfg = config(temp.path(), &url, "bypass", true);
    let mut pty = Pty::start(temp.path(), &cfg, &["chat"]);
    pty.until("> ");
    pty.send("start a command\n");
    pty.wait_for_model_signal(&entered_rx);
    let deadline = Instant::now() + Duration::from_secs(4);
    while !temp.path().join("marker").exists() {
        assert!(Instant::now() < deadline, "command did not start");
        std::thread::sleep(Duration::from_millis(10));
    }
    pty.send("/jobs\n");
    let listed = pty.until("exec");
    let job = listed
        .split_whitespace()
        .find(|item| item.starts_with("background-"))
        .unwrap()
        .to_owned();
    pty.send(&format!("/cancel-job {job}\n"));
    pty.until("Background cancellation requested.");
    release_tx.send(()).unwrap();
    pty.until("done");
    pty.until("> ");
    assert!(pty.finish().success());
}

#[test]
fn model_suggestion_never_switches_plan_mode_without_user_action() {
    let temp = TempDir::new().unwrap();
    let (url, _) = fixture::server(vec![(
        "application/json",
        answer("I recommend switching to BUILD for implementation."),
    )])
    .unwrap();
    let cfg = config(temp.path(), &url, "auto", true);
    let mut pty = Pty::start(temp.path(), &cfg, &["--mode", "plan", "chat"]);
    pty.until("> ");
    pty.send("Prepare a plan\n");
    pty.until("recommend switching to BUILD");
    pty.until("> ");
    pty.send("/status\n");
    let status = pty.until("permissions: auto");
    assert!(status.contains("mode: plan"), "{status}");
    pty.until("> ");
    assert!(pty.finish().success());
}

#[test]
fn switching_from_build_to_plan_during_model_call_blocks_generated_write() {
    let temp = TempDir::new().unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (url,requests)=fixture::server_dynamic(2,move |index,_| {
        if index==0 {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(8)).unwrap();
            let args=json!({"path":"blocked.txt","content":"no","expected_version":null}).to_string();
            ("application/json",json!({"choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"tool_calls":[{"id":"write","type":"function","function":{"name":"write_file","arguments":args}}]}}]}).to_string())
        } else {("application/json",answer("Plan retained; write was denied."))}
    }).unwrap();
    let cfg = config(temp.path(), &url, "auto", true);
    let mut pty = Pty::start(temp.path(), &cfg, &["--mode", "build", "chat"]);
    pty.until("> ");
    pty.send("Implement a file\n");
    pty.wait_for_model_signal(&entered_rx);
    pty.send("/mode plan\n");
    pty.until("pending plan");
    release_tx.send(()).unwrap();
    pty.until("Plan retained; write was denied.");
    pty.until("> ");
    pty.send("/status\n");
    let status = pty.until("permissions: auto");
    assert!(status.contains("mode: plan"));
    pty.until("> ");
    assert!(!temp.path().join("blocked.txt").exists());
    assert!(pty.finish().success());
    let _ = requests.recv_timeout(Duration::from_secs(3)).unwrap();
    let second = requests
        .recv_timeout(Duration::from_secs(3))
        .unwrap()
        .to_string();
    assert!(
        second.contains("mode_denied") || second.contains("permission_denied"),
        "{second}"
    );
}
