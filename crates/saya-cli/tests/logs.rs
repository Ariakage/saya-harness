#[path = "../../saya-core/tests/fixtures/http_support/mod.rs"]
mod fixture;
use saya_core::logging::{DiagnosticRecord, JsonLog, LogLevel};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};
use tempfile::TempDir;

struct Fixture {
    root: TempDir,
    config: PathBuf,
}
impl Fixture {
    fn new(url: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        fs::write(
            &config,
            format!(
                r#"schema_version = 1
[defaults]
profile = "test"
[profiles.test]
protocol = "openai-chat-completions"
base_url = "{url}"
model = "fake"
auth = "bearer"
api_key_env = "LOG_TEST_KEY"
context_window_tokens = 128000
stream = false
"#
            ),
        )
        .unwrap();
        Self { root, config }
    }
    fn state(&self) -> PathBuf {
        self.root.path().join("state/saya")
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_saya"));
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.root.path())
            .env("XDG_STATE_HOME", self.root.path().join("state"))
            .env("XDG_DATA_HOME", self.root.path().join("data"))
            .env("XDG_CACHE_HOME", self.root.path().join("cache"))
            .env("LOG_TEST_KEY", "fixture-log-secret")
            .current_dir(self.root.path())
            .arg("--config")
            .arg(&self.config)
            .stdin(Stdio::null());
        command
    }
    fn log(&self) -> JsonLog {
        JsonLog::new(self.state(), vec![])
    }
}
fn wait(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("log command did not exit");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn private_append(path: &Path, content: &[u8]) {
    fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(content)
        .unwrap();
}
#[test]
fn show_filters_pages_and_redacts_legacy_data_without_terminal_escapes() {
    let fixture = Fixture::new("http://127.0.0.1:9");
    let log = fixture.log();
    let mut record = DiagnosticRecord::new(LogLevel::Warn, "first");
    record.context.session_id = Some("session-A".into());
    record.context.run_id = Some("run-A".into());
    log.record_typed(&record).unwrap();
    log.record(
        "error",
        "legacy",
        json!({"session_id":"session-A","detail":"fixture-log-secret\u{1b}[31m\u{9b}31m"}),
    )
    .unwrap();
    private_append(&fixture.state().join("saya.log"), b"broken-tail");
    let output = fixture
        .command()
        .args([
            "--output",
            "json",
            "logs",
            "show",
            "--session",
            "session-A",
            "--level",
            "warn",
            "--limit",
            "1",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["records"][0]["event"], "first");
    assert_eq!(value["data"]["next_offset"], 1);
    let output = fixture
        .command()
        .args([
            "--output",
            "json",
            "logs",
            "show",
            "--session",
            "session-A",
            "--offset",
            "1",
        ])
        .output()
        .unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["records"][0]["event"], "legacy");
    assert_eq!(value["data"]["malformed_lines"], 1);
    assert!(
        !output
            .stdout
            .windows(b"fixture-log-secret".len())
            .any(|part| part == b"fixture-log-secret")
    );
    assert!(!output.stdout.contains(&0x1b));
    assert!(!String::from_utf8_lossy(&output.stdout).contains('\u{9b}'));
    assert!(String::from_utf8_lossy(&output.stdout).contains("[REDACTED]"));
    let output = fixture
        .command()
        .args(["--output", "jsonl", "logs", "show", "--since", "yesterday"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(!output.stdout.is_empty());
    for line in String::from_utf8(output.stdout).unwrap().lines() {
        let value: Value = serde_json::from_str(line).unwrap();
        assert!(value["error"].is_object());
    }
}
#[test]
fn tail_follows_rotation_and_interrupts_with_clean_jsonl() {
    let fixture = Fixture::new("http://127.0.0.1:9");
    let log = fixture.log();
    log.record("warn", "first", json!({})).unwrap();
    let mut child = fixture
        .command()
        .args(["--output", "jsonl", "logs", "tail", "--limit", "1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let first: Value =
        serde_json::from_str(&rx.recv_timeout(Duration::from_secs(4)).unwrap()).unwrap();
    assert_eq!(first["data"]["event"], "first");
    fs::rename(
        fixture.state().join("saya.log"),
        fixture.state().join("saya.log.1"),
    )
    .unwrap();
    log.record("error", "second", json!({})).unwrap();
    let second: Value =
        serde_json::from_str(&rx.recv_timeout(Duration::from_secs(4)).unwrap()).unwrap();
    assert_eq!(second["data"]["event"], "second");
    unsafe {
        libc::kill(child.id() as i32, libc::SIGINT);
    }
    assert_eq!(wait(&mut child).code(), Some(130));
    reader.join().unwrap();
    let output = fixture
        .command()
        .args(["--output", "json", "logs", "tail"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}
#[test]
fn query_rejects_symlinks_and_missing_logs_have_no_write_side_effects() {
    let fixture = Fixture::new("http://127.0.0.1:9");
    let output = fixture
        .command()
        .args(["--output", "json", "logs", "show"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!fixture.state().exists());
    fs::create_dir_all(fixture.state()).unwrap();
    let target = tempfile::NamedTempFile::new().unwrap();
    std::os::unix::fs::symlink(target.path(), fixture.state().join("saya.log")).unwrap();
    let output = fixture
        .command()
        .args(["--output", "json", "logs", "show"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(fs::read(target.path()).unwrap(), b"");
}
#[test]
fn invocation_level_overrides_environment_and_stdout_remains_one_result() {
    let body = json!({"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"private model text"}}]}).to_string();
    let (url, requests) = fixture::server(vec![
        ("application/json", body.clone()),
        ("application/json", body),
    ])
    .unwrap();
    let fixture = Fixture::new(&url);
    let first = fixture
        .command()
        .env("SAYA_LOG_LEVEL", "info")
        .args(["--output", "json", "run", "private user prompt"])
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let result: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(result["status"], "completed");
    let text = fs::read_to_string(fixture.state().join("saya.log")).unwrap();
    assert!(text.contains("model.started"));
    assert!(!text.contains("private user prompt"));
    assert!(!text.contains("private model text"));
    let second = fixture
        .command()
        .env("SAYA_LOG_LEVEL", "trace")
        .args([
            "--log-level",
            "error",
            "--output",
            "json",
            "run",
            "another question",
        ])
        .output()
        .unwrap();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let _: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(
        fs::read_to_string(fixture.state().join("saya.log")).unwrap(),
        text
    );
    requests.recv_timeout(Duration::from_secs(2)).unwrap();
    requests.recv_timeout(Duration::from_secs(2)).unwrap();
}

#[test]
fn slow_pipe_does_not_prevent_log_query_interrupts() {
    let fixture = Fixture::new("http://127.0.0.1:9");
    let log = fixture.log();
    for _ in 0..300 {
        log.record("warn", "large", json!({"detail":"x".repeat(8192)}))
            .unwrap();
    }
    for action in ["show", "tail"] {
        let mut child = fixture
            .command()
            .args(["--output", "jsonl", "logs", action, "--limit", "1000"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // Observe the first byte to synchronize with process startup, then keep
        // the remaining pipe data unread to exercise real backpressure.
        use std::os::fd::AsRawFd;
        let stdout = child.stdout.as_mut().unwrap();
        let descriptor = stdout.as_raw_fd();
        unsafe {
            libc::fcntl(descriptor, libc::F_SETFL, libc::O_NONBLOCK);
        }
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            match stdout.read(&mut [0_u8; 1]) {
                Ok(1) => break,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5))
                }
                other => {
                    child.kill().unwrap();
                    panic!("log startup output unavailable {other:?}");
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
        unsafe {
            libc::kill(child.id() as i32, libc::SIGINT);
        }
        assert_eq!(wait(&mut child).code(), Some(130));
    }
}
