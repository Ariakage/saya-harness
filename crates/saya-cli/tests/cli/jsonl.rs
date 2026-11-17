use crate::common::*;
use crate::fixture;
#[allow(unused_imports)]
use serde_json::json;
#[allow(unused_imports)]
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};
use tempfile::TempDir;

#[test]
fn json_run_emits_one_complete_result_without_text_stream_pollution() {
    let temp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let (url, _) = fixture::server(vec![(
        "application/json",
        response(
            json!({"role":"assistant","content":"machine answer"}),
            "stop",
        ),
    )])
    .unwrap();
    let cfg = config(temp.path(), &url, "auto");
    let output = command(temp.path(), home.path())
        .arg("--config")
        .arg(cfg)
        .args(["--output", "json", "run", "hello"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["status"], "completed");
    assert_eq!(value["text"], "machine answer");
    assert!(value["session_id"].is_string());
    assert!(value["run_id"].is_string());
    for key in ["stop_reason", "error", "usage", "artifacts", "persistence"] {
        assert!(value.get(key).is_some(), "missing {key}");
    }
    assert!(!output.stdout.contains(&0x1b));
}

#[test]
fn json_noninteractive_confirmation_and_config_error_have_machine_results() {
    let temp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let (url, _) = fixture::server(vec![("application/json", read_call())]).unwrap();
    fs::write(temp.path().join("input.txt"), "data").unwrap();
    let cfg = config(temp.path(), &url, "manual");
    let output = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["--output", "json", "run", "read"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["status"], "needs_confirmation");
    let failed = command(temp.path(), home.path())
        .args([
            "--output",
            "json",
            "--config",
            "nonexistent.toml",
            "run",
            "test",
        ])
        .output()
        .unwrap();
    assert_eq!(failed.status.code(), Some(2));
    let value: serde_json::Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(value["status"], "failed");
    assert!(value["session_id"].is_null());
    assert!(value["run_id"].is_null());
}

#[test]
fn jsonl_run_emits_ordered_events_and_one_durable_final_result() {
    let temp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    fs::write(temp.path().join("input.txt"), "test data").unwrap();
    let (url, _) = fixture::server(vec![
        ("application/json", read_call()),
        (
            "application/json",
            response(json!({"role":"assistant","content":"finished"}), "stop"),
        ),
    ])
    .unwrap();
    let cfg = config(temp.path(), &url, "auto");
    let output = command(temp.path(), home.path())
        .arg("--config")
        .arg(cfg)
        .args(["--output", "jsonl", "run", "read"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let kinds: Vec<&str> = events
        .iter()
        .map(|event| event["type"].as_str().unwrap())
        .collect();
    assert_eq!(kinds.first(), Some(&"session.created"));
    assert_eq!(kinds.last(), Some(&"run.finished"));
    assert_eq!(
        kinds.iter().filter(|kind| **kind == "run.finished").count(),
        1
    );
    for kind in [
        "run.started",
        "model.started",
        "model.finished",
        "tool.requested",
        "tool.started",
        "tool.finished",
    ] {
        assert!(kinds.contains(&kind), "missing {kind}: {kinds:?}");
    }
    for pair in events.windows(2) {
        assert!(pair[0]["seq"].as_u64().unwrap() < pair[1]["seq"].as_u64().unwrap());
    }
    assert_eq!(events.last().unwrap()["data"]["text"], "finished");
    assert_eq!(events.last().unwrap()["data"]["status"], "completed");
    assert!(
        events.iter().all(|event| event["schema_version"] == 1
            && event["timestamp"].as_str().unwrap().ends_with('Z'))
    );
    let db =
        rusqlite::Connection::open(home.path().join(".local/share/saya/sessions.sqlite3")).unwrap();
    let count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM events WHERE json_extract(event,'$.type')='run.finished'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn jsonl_reports_noninteractive_approval_and_clarification() {
    for (permissions, tool, status, required) in [
        (
            "manual",
            "read_file",
            "needs_confirmation",
            "permission.requested",
        ),
        ("auto", "ask_user", "needs_input", "input.requested"),
    ] {
        let temp = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        fs::write(temp.path().join("input.txt"), "data").unwrap();
        let body = if tool == "read_file" {
            read_call()
        } else {
            tool_response(
                "ask",
                "ask_user",
                json!({"question":"Which option?","options":["one","two"]}),
            )
        };
        let (url, _) = fixture::server(vec![("application/json", body)]).unwrap();
        let cfg = config(temp.path(), &url, permissions);
        let output = command(temp.path(), home.path())
            .arg("--config")
            .arg(cfg)
            .args(["--output", "jsonl", "run", "task"])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let lines: Vec<serde_json::Value> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.last().unwrap()["data"]["status"], status, "{lines:?}");
        assert!(
            lines.iter().any(|event| event["type"] == required),
            "{lines:?}"
        );
        if status == "needs_confirmation" {
            assert!(
                !lines
                    .iter()
                    .any(|event| event["type"] == "tool.started"
                        && event["data"]["call_id"] == "call1"),
                "{lines:?}"
            );
        }
        assert!(
            lines
                .iter()
                .any(|event| event["type"] == "permission.reviewed"),
            "{lines:?}"
        );
        assert_eq!(
            output.status.code(),
            Some(if status == "needs_input" { 4 } else { 3 })
        );
    }
}

#[test]
fn jsonl_config_error_returns_one_terminal_event() {
    let temp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let output = command(temp.path(), home.path())
        .args([
            "--output",
            "jsonl",
            "--config",
            "missing.toml",
            "run",
            "task",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["type"], "run.finished");
    assert_eq!(events[0]["data"]["status"], "failed");
    assert!(events[0]["session_id"].is_null());
}

#[test]
fn machine_modes_preserve_streamed_text_and_jsonl_sequence_across_runs() {
    for output_mode in ["json", "jsonl"] {
        let temp = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        let stream = || {
            fixture::sse(&[
                json!({"choices":[{"index":0,"delta":{"content":"hello "},"finish_reason":null}]}),
                json!({"choices":[{"index":0,"delta":{"content":"world"},"finish_reason":null}]}),
                json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
            ])
        };
        let (url, _) = fixture::server(vec![
            ("text/event-stream", stream()),
            ("text/event-stream", stream()),
        ])
        .unwrap();
        let cfg = config(temp.path(), &url, "auto");
        let source = fs::read_to_string(&cfg)
            .unwrap()
            .replace("stream = false", "stream = true");
        fs::write(&cfg, source).unwrap();
        let first = command(temp.path(), home.path())
            .arg("--config")
            .arg(&cfg)
            .args(["--output", output_mode, "run", "first"])
            .output()
            .unwrap();
        assert!(
            first.status.success(),
            "{}",
            String::from_utf8_lossy(&first.stderr)
        );
        if output_mode == "json" {
            let result: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
            assert_eq!(result["text"], "hello world");
            continue;
        }
        let events: Vec<serde_json::Value> = String::from_utf8(first.stdout)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        let visible: String = events
            .iter()
            .filter(|e| e["type"] == "model.text_delta")
            .map(|e| e["data"]["text"].as_str().unwrap())
            .collect();
        assert_eq!(visible, "hello world");
        let session = events.last().unwrap()["data"]["session_id"]
            .as_str()
            .unwrap();
        let last_seq = events.last().unwrap()["seq"].as_u64().unwrap();
        let resumed = command(temp.path(), home.path())
            .arg("--config")
            .arg(&cfg)
            .args(["--output", "jsonl", "run", "--session", session, "second"])
            .output()
            .unwrap();
        assert!(
            resumed.status.success(),
            "{}",
            String::from_utf8_lossy(&resumed.stderr)
        );
        let resumed_events: Vec<serde_json::Value> = String::from_utf8(resumed.stdout)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(resumed_events[0]["type"], "session.resumed");
        assert!(resumed_events[0]["seq"].as_u64().unwrap() > last_seq);
    }
}

#[test]
fn jsonl_interrupted_model_has_matching_finish_and_cancelled_result() {
    use std::net::TcpListener;
    let temp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut data = [0u8; 2048];
            let _ = stream.read(&mut data);
            let _ = entered_tx.send(());
            let _ = release_rx.recv_timeout(Duration::from_secs(30));
        }
    });
    let cfg = config(temp.path(), &format!("http://{address}/v1"), "auto");
    let child = command(temp.path(), home.path())
        .arg("--config")
        .arg(cfg)
        .args(["--output", "jsonl", "run", "wait"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    entered_rx.recv_timeout(Duration::from_secs(20)).unwrap();
    // SAFETY: child is a live process spawned by this test.
    assert_eq!(
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) },
        0
    );
    let output = child.wait_with_output().unwrap();
    let _ = release_tx.send(());
    assert_eq!(
        output.status.code(),
        Some(130),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "model.started")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "model.finished")
            .count(),
        1
    );
    assert!(
        events
            .iter()
            .any(|e| e["type"] == "model.finished" && e["data"]["ok"] == false)
    );
    assert_eq!(events.last().unwrap()["data"]["status"], "cancelled");
}

#[test]
fn configured_json_output_covers_prompt_errors_and_noninteractive_questions() {
    let temp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let (url, _) = fixture::server(vec![(
        "application/json",
        tool_response(
            "ask",
            "ask_user",
            json!({"question":"Which option?","options":["one","two"]}),
        ),
    )])
    .unwrap();
    let cfg = config(temp.path(), &url, "auto");
    let contents = fs::read_to_string(&cfg).unwrap().replace(
        "profile = \"test\"",
        "profile = \"test\"\noutput = \"json\"",
    );
    fs::write(&cfg, contents).unwrap();
    let empty = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["run", " "])
        .output()
        .unwrap();
    assert_eq!(empty.status.code(), Some(2));
    let result: serde_json::Value = serde_json::from_slice(&empty.stdout).unwrap();
    assert_eq!(result["status"], "failed");
    let question = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["run", "choose"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(question.status.code(), Some(4));
    let result: serde_json::Value = serde_json::from_slice(&question.stdout).unwrap();
    assert_eq!(result["status"], "needs_input");
    assert_eq!(result["text"], "Which option?");
}

#[test]
fn jsonl_interactive_approval_uses_tty_and_reports_resolution() {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::process::CommandExt,
    };
    const PTY_TIMEOUT_MS: libc::c_int = 15_000;
    const SETTLE_TIMEOUT: Duration = Duration::from_millis(PTY_TIMEOUT_MS as u64);
    let temp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    fs::write(temp.path().join("input.txt"), "data").unwrap();
    let (url, _) = fixture::server(vec![
        ("application/json", read_call()),
        (
            "application/json",
            tool_response(
                "ask",
                "ask_user",
                json!({"question":"Which option?","options":["one","two"]}),
            ),
        ),
        (
            "application/json",
            response(json!({"role":"assistant","content":"done"}), "stop"),
        ),
    ])
    .unwrap();
    let cfg = config(temp.path(), &url, "manual");
    let mut master = -1;
    let mut slave = -1;
    // SAFETY: descriptors point to writable storage and optional arguments are null.
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
    // SAFETY: this fixture owns master; child processes must not retain its peer.
    assert_eq!(
        unsafe { libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC) },
        0
    );
    let mut master = unsafe { fs::File::from_raw_fd(master) };
    let slave = unsafe { fs::File::from_raw_fd(slave) };
    let fd = slave.as_raw_fd();
    let mut cmd = command(temp.path(), home.path());
    cmd.arg("--config")
        .arg(cfg)
        .args(["--output", "jsonl", "run", "--interactive", "read"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(slave.try_clone().unwrap()));
    // SAFETY: only async-signal-safe syscalls run before exec.
    unsafe {
        cmd.pre_exec(move || {
            if libc::setsid() < 0 || libc::ioctl(fd, libc::TIOCSCTTY as _, 0) < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = cmd.spawn().unwrap();
    drop(cmd);
    drop(slave);
    let stdout = child.stdout.take().unwrap();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        use std::io::BufRead;
        let mut stdout = std::io::BufReader::new(stdout);
        let mut data = String::new();
        loop {
            let mut line = String::new();
            if stdout.read_line(&mut line).unwrap() == 0 {
                break;
            }
            let finished = serde_json::from_str::<serde_json::Value>(&line)
                .ok()
                .is_some_and(|v| v["type"] == "run.finished");
            data.push_str(&line);
            if finished {
                let _ = finished_tx.send(());
            }
        }
        data
    });
    let mut tty = Vec::new();
    while !String::from_utf8_lossy(&tty).contains("[y/N]") {
        let mut poll = libc::pollfd {
            fd: master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one owned PTY descriptor and bounded timeout.
        assert!(unsafe { libc::poll(&mut poll, 1, PTY_TIMEOUT_MS) } > 0);
        let mut buf = [0u8; 1024];
        let n = master.read(&mut buf).unwrap();
        assert!(n > 0);
        tty.extend_from_slice(&buf[..n]);
    }
    master.write_all(b"y\n").unwrap();
    fn await_tty(master: &mut fs::File, marker: &str) {
        let mut data = Vec::new();
        while !String::from_utf8_lossy(&data).contains(marker) {
            let mut poll = libc::pollfd {
                fd: master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: poll receives a live owned descriptor and a finite timeout.
            assert!(unsafe { libc::poll(&mut poll, 1, PTY_TIMEOUT_MS) } > 0);
            let mut buf = [0u8; 1024];
            let n = master.read(&mut buf).unwrap();
            assert!(n > 0);
            data.extend_from_slice(&buf[..n]);
        }
    }
    await_tty(&mut master, "[y/N]");
    master.write_all(b"y\n").unwrap();
    await_tty(&mut master, "Answer:");
    master.write_all(b"two\n").unwrap();
    if finished_rx.recv_timeout(SETTLE_TIMEOUT).is_err() {
        drop(master);
        let _ = child.kill();
        panic!("interactive JSONL final event did not arrive");
    }
    drop(master);
    let deadline = std::time::Instant::now() + SETTLE_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("interactive JSONL run did not exit");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success(), "{status:?}");
    let text = reader.join().unwrap();
    assert!(!text.contains("Allow this call?"));
    let events: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "permission.resolved"
                && event["data"]["source"] == "user"
                && event["data"]["decision"] == "allow"),
        "{events:?}"
    );
    assert!(
        events.iter().any(|event| event["type"] == "input.requested"
            && event["data"]["question"] == "Which option?"),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "input.received" && event["data"]["answer"] == "two"),
        "{events:?}"
    );
}

#[test]
fn jsonl_never_emits_a_known_secret_split_across_stream_chunks() {
    let temp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let secret = "fixture-secret-12345";
    let body = fixture::sse(&[
        json!({"choices":[{"index":0,"delta":{"content":"fixture-secret-"},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{"content":"12345"},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
    ]);
    let (url, _) = fixture::server(vec![("text/event-stream", body)]).unwrap();
    let cfg = config(temp.path(), &url, "auto");
    let contents = fs::read_to_string(&cfg)
        .unwrap()
        .replace("stream = false", "stream = true")
        .replace(
            "auth = \"none\"",
            "auth = \"bearer\"\napi_key_env = \"TEST_KEY\"",
        );
    fs::write(&cfg, contents).unwrap();
    let output = command(temp.path(), home.path())
        .env("TEST_KEY", secret)
        .arg("--config")
        .arg(cfg)
        .args(["--output", "jsonl", "run", "test"])
        .output()
        .unwrap();
    let out = String::from_utf8(output.stdout).unwrap();
    let err = String::from_utf8(output.stderr).unwrap();
    assert!(!out.contains(secret));
    assert!(!err.contains(secret));
    assert!(out.contains("[REDACTED]"), "{out}");
    let events: Vec<serde_json::Value> = out
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "run.finished")
            .count(),
        1
    );
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn jsonl_reports_result_persistence_failure_once_without_replaying_write() {
    let temp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let (url, _) = fixture::server(vec![
        (
            "application/json",
            response(json!({"role":"assistant","content":"first"}), "stop"),
        ),
        (
            "application/json",
            tool_response(
                "write-once",
                "write_file",
                json!({"path":"once.txt","content":"once","expected_version":null}),
            ),
        ),
    ])
    .unwrap();
    let cfg = config(temp.path(), &url, "auto");
    assert!(
        command(temp.path(), home.path())
            .arg("--config")
            .arg(&cfg)
            .args(["run", "start"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let list = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["--output", "json", "session", "list"])
        .output()
        .unwrap();
    let list: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let id = list["data"][0]["id"].as_str().unwrap();
    let db =
        rusqlite::Connection::open(home.path().join(".local/share/saya/sessions.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_result BEFORE UPDATE OF result ON tool_calls WHEN NEW.call_id='write-once' BEGIN SELECT RAISE(ABORT,'test failure'); END;").unwrap();
    let result = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["--output", "jsonl", "run", "--session", id, "write"])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(
        fs::read_to_string(temp.path().join("once.txt")).unwrap(),
        "once"
    );
    assert!(String::from_utf8_lossy(&result.stderr).contains("observed tool result (not saved)"));
    let events: Vec<serde_json::Value> = String::from_utf8(result.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "run.finished")
            .count(),
        1
    );
    let final_result = &events.last().unwrap()["data"];
    assert_eq!(final_result["status"], "failed");
    assert_eq!(final_result["persistence"]["saved"], false);
    assert_eq!(final_result["error"]["code"], "storage_error");
}

#[test]
fn json_session_query_failure_uses_query_envelope_without_run_event() {
    let temp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let cfg = config(temp.path(), "http://127.0.0.1:1/v1", "auto");
    let output = command(temp.path(), home.path())
        .arg("--config")
        .arg(cfg)
        .args(["--output", "json", "session", "show", "invalid-id"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema_version"], 1);
    assert!(value["data"].is_null());
    assert!(value["error"].is_object());
    assert!(value.get("run_id").is_none());
    assert!(value.get("type").is_none());
}

#[test]
fn run_reports_known_usage_metrics_and_verified_file_artifact() {
    let temp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let with_usage = |message: serde_json::Value, finish: &str, input: u64, output: u64| {
        json!({"choices":[{"index":0,"finish_reason":finish,"message":message}],"usage":{"prompt_tokens":input,"completion_tokens":output}}).to_string()
    };
    let arguments =
        json!({"path":"artifact.txt","content":"result","expected_version":null}).to_string();
    let call = json!({"role":"assistant","content":null,"tool_calls":[{"id":"write","type":"function","function":{"name":"write_file","arguments":arguments}}]});
    let (url, _) = fixture::server(vec![
        ("application/json", with_usage(call, "tool_calls", 10, 5)),
        (
            "application/json",
            with_usage(json!({"role":"assistant","content":"done"}), "stop", 4, 2),
        ),
    ])
    .unwrap();
    let cfg = config(temp.path(), &url, "auto");
    let mut config_text = fs::read_to_string(&cfg).unwrap();
    config_text.push_str("\n[profiles.test.pricing]\ncurrency = \"USD\"\ninput_microunits_per_million = 1000000\noutput_microunits_per_million = 2000000\n");
    fs::write(&cfg, config_text).unwrap();
    let output = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["--output", "json", "run", "write"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result["usage"],
        json!({"input_tokens":14,"output_tokens":7,"estimated":false})
    );
    assert_eq!(result["artifacts"].as_array().unwrap().len(), 1);
    assert_eq!(result["artifacts"][0]["operation"], "created");
    assert_eq!(
        result["artifacts"][0]["path"],
        fs::canonicalize(temp.path().join("artifact.txt"))
            .unwrap()
            .to_str()
            .unwrap()
    );
    let id = result["session_id"].as_str().unwrap();
    let show = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["--output", "json", "session", "show", id])
        .output()
        .unwrap();
    let show: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    let metrics = &show["data"]["run_metrics"][0]["metrics"];
    assert_eq!(metrics["execution_requests"], 2);
    assert_eq!(metrics["review_requests"], 0);
    assert_eq!(metrics["tool_calls"], 1);
    assert_eq!(metrics["execution_usage"]["input_tokens"], 14);
    assert_eq!(
        metrics["execution_cost"],
        json!({"currency":"USD","microunits":28,"rounded_up":false,"estimated":true})
    );
    assert!(metrics["review_cost"].is_null());
    assert!(metrics["duration_ms"].is_u64());
}

#[test]
fn run_keeps_partial_provider_usage_unknown_and_separates_review() {
    let temp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let args =
        json!({"argv":["/usr/bin/touch","command-artifact"],"cwd":null,"timeout_seconds":null})
            .to_string();
    let call = json!({"role":"assistant","content":null,"tool_calls":[{"id":"cmd","type":"function","function":{"name":"exec","arguments":args}}]});
    let first=json!({"choices":[{"index":0,"finish_reason":"tool_calls","message":call}],"usage":{"prompt_tokens":10,"completion_tokens":5}}).to_string();
    let review=json!({"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"{\"decision\":\"allow\",\"reason\":\"safe\"}"}}],"usage":{"prompt_tokens":3,"completion_tokens":2}}).to_string();
    let last = response(json!({"role":"assistant","content":"done"}), "stop");
    let (url, _) = fixture::server(vec![
        ("application/json", first),
        ("application/json", review),
        ("application/json", last),
    ])
    .unwrap();
    let cfg = config(temp.path(), &url, "auto");
    let output = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["--output", "json", "run", "command"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result["usage"],
        json!({"input_tokens":null,"output_tokens":null,"estimated":true})
    );
    assert!(result["artifacts"].as_array().unwrap().is_empty());
    assert!(temp.path().join("command-artifact").exists());
    let id = result["session_id"].as_str().unwrap();
    let show = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["--output", "json", "session", "show", id])
        .output()
        .unwrap();
    let show: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    let metrics = &show["data"]["run_metrics"][0]["metrics"];
    assert_eq!(metrics["execution_requests"], 2);
    assert_eq!(metrics["review_requests"], 1);
    assert_eq!(metrics["review_usage"]["input_tokens"], 3);
    assert!(metrics["execution_usage"]["input_tokens"].is_null());
}

#[test]
fn credentials_are_redacted_from_model_tool_results_session_and_text_output() {
    let temp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let secret = "fake-runtime-secret-123";
    fs::write(
        temp.path().join("input.txt"),
        format!("prefix {secret} suffix"),
    )
    .unwrap();
    fs::write(
        temp.path().join("AGENTS.md"),
        format!("project instruction {secret}"),
    )
    .unwrap();
    let (url, requests) = fixture::server(vec![
        ("application/json", read_call()),
        (
            "application/json",
            response(
                json!({"role":"assistant","content":format!("answer {secret}")}),
                "stop",
            ),
        ),
    ])
    .unwrap();
    let cfg = config(temp.path(), &url, "auto");
    let contents = fs::read_to_string(&cfg).unwrap().replace(
        "auth = \"none\"",
        "auth = \"bearer\"\napi_key_env = \"TEST_KEY\"",
    );
    fs::write(&cfg, contents).unwrap();
    let output = command(temp.path(), home.path())
        .env("TEST_KEY", secret)
        .arg("--config")
        .arg(&cfg)
        .args(["run", &format!("inspect {secret}")])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "answer [REDACTED]\n"
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
    let first = requests
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .to_string();
    let second = requests
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .to_string();
    assert!(!first.contains(secret));
    assert!(!second.contains(secret));
    assert!(second.contains("[REDACTED]"));
    let list = command(temp.path(), home.path())
        .env("TEST_KEY", secret)
        .arg("--config")
        .arg(&cfg)
        .args(["--output", "json", "session", "list"])
        .output()
        .unwrap();
    let list: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let id = list["data"][0]["id"].as_str().unwrap();
    let show = command(temp.path(), home.path())
        .env("TEST_KEY", secret)
        .arg("--config")
        .arg(&cfg)
        .args(["--output", "json", "session", "show", id])
        .output()
        .unwrap();
    let show = String::from_utf8(show.stdout).unwrap();
    assert!(!show.contains(secret));
    assert!(show.contains("[REDACTED]"));
    for entry in fs::read_dir(home.path().join(".local/share/saya")).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            let bytes = fs::read(path).unwrap();
            assert!(
                !bytes
                    .windows(secret.len())
                    .any(|window| window == secret.as_bytes())
            );
        }
    }
}

#[test]
fn config_inside_workspace_is_protected_from_file_and_process_tools() {
    let workspace = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let cfg = config(workspace.path(), "http://127.0.0.1:1/v1", "bypass");
    let mut contents = fs::read_to_string(&cfg).unwrap();
    contents.push_str("# PROTECTED_CONFIG_SENTINEL\n");
    fs::write(&cfg, contents).unwrap();
    let read = tool_response(
        "read-config",
        "read_file",
        json!({"path":"config.toml","start_line":1,"line_count":100}),
    );
    let overwrite = tool_response(
        "overwrite-config",
        "exec",
        json!({
            "argv":["/bin/sh","-c","printf overwritten > config.toml"],
            "cwd":null,
            "timeout_seconds":null
        }),
    );
    let (url, requests) = fixture::server(vec![
        ("application/json", read),
        ("application/json", overwrite),
        (
            "application/json",
            response(json!({"role":"assistant","content":"done"}), "stop"),
        ),
    ])
    .unwrap();
    fs::write(
        &cfg,
        fs::read_to_string(&cfg)
            .unwrap()
            .replace("http://127.0.0.1:1/v1", &url),
    )
    .unwrap();
    let original = fs::read(&cfg).unwrap();
    let result = command(workspace.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["run", "inspect protected config"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(fs::read(&cfg).unwrap(), original);
    let requests = requests.try_iter().collect::<Vec<_>>();
    assert_eq!(requests.len(), 3);
    assert!(
        requests
            .iter()
            .all(|request| !request.to_string().contains("PROTECTED_CONFIG_SENTINEL"))
    );
}

#[test]
fn completed_background_write_is_included_in_artifacts() {
    let temp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let marker = temp.path().join("background-artifact.txt");
    let mut job_id = None;
    let mut deadline = None;
    let (url, _) = fixture::server_dynamic(64, move |index, request| {
        if index == 0 {
            return (
                "application/json",
                tool_response(
                    "launch",
                    "background_start",
                    json!({"tool":"write_file","arguments":{"path":"background-artifact.txt","content":"done","expected_version":null}}),
                ),
            );
        }
        let message = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|message| message["role"] == "tool")
            .unwrap();
        let result: serde_json::Value =
            serde_json::from_str(message["content"].as_str().unwrap()).unwrap();
        assert_eq!(result["ok"], true, "{result}");
        if index == 1 {
            assert_eq!(message["tool_call_id"], "launch");
            assert_eq!(result["data"]["status"], "started", "{result}");
            job_id = Some(result["data"]["job_id"].as_str().unwrap().to_owned());
            deadline = Some(std::time::Instant::now() + Duration::from_secs(10));
        } else {
            assert_eq!(message["tool_call_id"], format!("status-{}", index - 1));
            if result["data"]["status"] == "finished" {
                assert_eq!(result["data"]["result"]["ok"], true, "{result}");
                return (
                    "application/json",
                    response(json!({"role":"assistant","content":"done"}), "stop"),
                );
            }
            assert_eq!(result["data"]["status"], "running", "{result}");
        }
        assert!(index < 63, "background write exceeded the model request bound");
        assert!(
            std::time::Instant::now() < deadline.unwrap(),
            "background write did not finish before the polling deadline"
        );
        std::thread::sleep(Duration::from_millis(100));
        (
            "application/json",
            tool_response(
                &format!("status-{index}"),
                "background_status",
                json!({"job_id":job_id.as_ref().unwrap()}),
            ),
        )
    })
    .unwrap();
    let cfg = config(temp.path(), &url, "auto");
    let output = command(temp.path(), home.path())
        .arg("--config")
        .arg(cfg)
        .args(["--output", "json", "run", "start"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_ne!(
        value["stop_reason"], "completed_with_background_cleanup",
        "{value}"
    );
    assert_eq!(value["artifacts"].as_array().unwrap().len(), 1, "{value}");
    assert_eq!(
        value["artifacts"][0]["path"],
        fs::canonicalize(marker).unwrap().to_str().unwrap()
    );
}

#[test]
fn warnings_are_logged_as_redacted_json_without_stdout_pollution() {
    let temp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let secret = "fake-logging-secret";
    fs::write(temp.path().join("input.txt"), "data").unwrap();
    let (url, _) = fixture::server(vec![("application/json", read_call())]).unwrap();
    let cfg = config(temp.path(), &url, "manual");
    let config = fs::read_to_string(&cfg).unwrap().replace(
        "auth = \"none\"",
        "auth = \"bearer\"\napi_key_env = \"TEST_KEY\"",
    );
    fs::write(&cfg, config).unwrap();
    let output = command(temp.path(), home.path())
        .env("TEST_KEY", secret)
        .arg("--config")
        .arg(cfg)
        .args(["--output", "json", "run", &format!("read {secret}")])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "needs_confirmation");
    let path = home.path().join(".local/state/saya/saya.log");
    let contents = fs::read_to_string(path).unwrap();
    assert!(!contents.contains(secret));
    let line: serde_json::Value = serde_json::from_str(contents.trim()).unwrap();
    assert_eq!(line["level"], "warn");
    assert_eq!(line["event"], "run.finished");
    assert_eq!(line["data"]["status"], "needs_confirmation");
}

#[test]
fn jsonl_terminal_failure_uses_reserved_identity_and_sequence_when_database_reservation_fails() {
    for fail in [false, true] {
        let temp = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        let database = home.path().join(".local/share/saya/sessions.sqlite3");
        let trigger_database = database.clone();
        let (url, _) = fixture::server_dynamic(2, move |index, _| {
            if index > 0 {
                return ("text/event-stream", fixture::sse(&[
                    json!({"choices":[{"index":0,"delta":{"content":"resumed"},"finish_reason":null}]}),
                    json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
                ]));
            }
            if fail {
                let db = rusqlite::Connection::open(&trigger_database).unwrap();
                db.execute_batch("CREATE TRIGGER reject_reservation BEFORE UPDATE OF event_seq ON sessions BEGIN SELECT RAISE(ABORT,'controlled reservation failure'); END;").unwrap();
            }
            let mut chunks = vec![json!({"choices":[{"index":0,"delta":{"content":"x"},"finish_reason":null}]}); 4100];
            chunks.push(json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}));
            ("text/event-stream", fixture::sse(&chunks))
        }).unwrap();
        let cfg = config(temp.path(), &url, "auto");
        fs::write(
            &cfg,
            fs::read_to_string(&cfg)
                .unwrap()
                .replace("stream = false", "stream = true"),
        )
        .unwrap();
        let output = command(temp.path(), home.path())
            .arg("--config")
            .arg(&cfg)
            .args(["--output", "jsonl", "run", "sequence fixture"])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(if fail { 1 } else { 0 }),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let events = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            events
                .iter()
                .filter(|event| event["type"] == "run.finished")
                .count(),
            1
        );
        assert!(
            events
                .windows(2)
                .all(|pair| pair[0]["seq"].as_u64().unwrap() < pair[1]["seq"].as_u64().unwrap())
        );
        let final_event = events.last().unwrap();
        let session = events[0]["session_id"].as_str().unwrap();
        let run = &events
            .iter()
            .find(|event| event["type"] == "run.started")
            .unwrap()["run_id"];
        assert_eq!(final_event["session_id"], session);
        assert_eq!(&final_event["run_id"], run);
        assert_eq!(&final_event["data"]["run_id"], run);
        if fail {
            assert_eq!(final_event["seq"], 4096);
            assert_eq!(final_event["data"]["status"], "failed");
            assert_eq!(final_event["data"]["persistence"]["saved"], false);
            let db = rusqlite::Connection::open(database).unwrap();
            db.execute_batch("DROP TRIGGER reject_reservation;")
                .unwrap();
        } else {
            assert!(final_event["seq"].as_u64().unwrap() > 4096);
        }
        let resumed = command(temp.path(), home.path())
            .arg("--config")
            .arg(&cfg)
            .args(["--output", "jsonl", "run", "--session", session, "continue"])
            .output()
            .unwrap();
        assert!(
            resumed.status.success(),
            "fail={fail} stderr={} stdout={}",
            String::from_utf8_lossy(&resumed.stderr),
            String::from_utf8_lossy(&resumed.stdout)
        );
        let text = String::from_utf8(resumed.stdout).unwrap();
        let first: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert!(first["seq"].as_u64().unwrap() > final_event["seq"].as_u64().unwrap());
    }
}
