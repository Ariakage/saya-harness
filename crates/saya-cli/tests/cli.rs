#[path = "../../saya-core/tests/fixtures/http_support/mod.rs"]
mod fixture;
use serde_json::json;
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};
use tempfile::TempDir;

fn command(root: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_saya"));
    cmd.env_clear()
        .env("HOME", root)
        .env("PATH", "/usr/bin:/bin")
        .current_dir(root);
    cmd
}
fn config(root: &Path, url: &str, permissions: &str) -> std::path::PathBuf {
    let p = root.join("config.toml");
    fs::write(
        &p,
        format!(
            r#"schema_version = 1
[defaults]
profile = "test"
permissions = "{permissions}"
[profiles.test]
protocol = "openai-chat-completions"
base_url = "{url}"
model = "fake"
auth = "none"
context_window_tokens = 128000
stream = false
"#
        ),
    )
    .unwrap();
    p
}
fn response(message: serde_json::Value, finish: &str) -> String {
    json!({"choices":[{"index":0,"finish_reason":finish,"message":message}]}).to_string()
}
fn read_call() -> String {
    response(
        json!({"role":"assistant","content":null,"tool_calls":[{"id":"call1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"input.txt\",\"start_line\":1,\"line_count\":10}"}}]}),
        "tool_calls",
    )
}

#[test]
fn config_init_never_overwrites_and_check_accepts_empty_template() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("config.toml");
    let run = || {
        command(temp.path())
            .args(["--config", path.to_str().unwrap(), "config", "init"])
            .output()
            .unwrap()
    };
    assert!(run().status.success());
    let before = fs::read(&path).unwrap();
    assert!(!run().status.success());
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(
        command(temp.path())
            .args(["--config", path.to_str().unwrap(), "config", "check"])
            .output()
            .unwrap()
            .status
            .success()
    );
}
#[test]
fn prompt_sources_are_explicit_mutually_exclusive_and_nonempty() {
    let temp = TempDir::new().unwrap();
    let cfg = config(temp.path(), "http://127.0.0.1:1/v1", "auto");
    for args in [
        vec!["run"],
        vec!["run", " "],
        vec!["run", "hello", "--stdin"],
    ] {
        let result = command(temp.path())
            .arg("--config")
            .arg(&cfg)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(2));
    }
    let mut child = command(temp.path())
        .arg("--config")
        .arg(&cfg)
        .args(["run", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"  \n").unwrap();
    assert_eq!(child.wait_with_output().unwrap().status.code(), Some(2));
}
#[test]
fn local_model_can_read_workspace_but_never_receives_mutation_tools() {
    let temp = TempDir::new().unwrap();
    fs::write(temp.path().join("input.txt"), "fixture data").unwrap();
    let (url, requests) = fixture::server(vec![
        ("application/json", read_call()),
        (
            "application/json",
            response(json!({"role":"assistant","content":"finished"}), "stop"),
        ),
    ])
    .unwrap();
    let cfg = config(temp.path(), &url, "auto");
    let mut child = command(temp.path())
        .arg("--config")
        .arg(cfg)
        .args(["run", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"inspect the input\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "finished\n");
    let first = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    let names: Vec<_> = first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["function"]["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"read_file"));
    assert!(!names.contains(&"write_file"));
    assert!(!names.contains(&"exec"));
    let second = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(second.to_string().contains("fixture data"));
}
#[test]
fn manual_noninteractive_read_needs_confirmation() {
    let temp = TempDir::new().unwrap();
    fs::write(temp.path().join("input.txt"), "data").unwrap();
    let (url, _) = fixture::server(vec![("application/json", read_call())]).unwrap();
    let cfg = config(temp.path(), &url, "manual");
    let result = command(temp.path())
        .arg("--config")
        .arg(cfg)
        .args(["run", "read"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&result.stderr).contains("needs_confirmation"));
}

#[test]
fn interactive_approval_uses_controlling_tty_separate_from_prompt_stdin() {
    use std::{
        io::Read,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::process::CommandExt,
        },
    };
    let temp = TempDir::new().unwrap();
    fs::write(temp.path().join("input.txt"), "data").unwrap();
    let (url, requests) = fixture::server(vec![
        ("application/json", read_call()),
        (
            "application/json",
            response(json!({"role":"assistant","content":"approved"}), "stop"),
        ),
    ])
    .unwrap();
    let cfg = config(temp.path(), &url, "manual");
    let mut master = -1;
    let mut slave = -1;
    // SAFETY: Both integer pointers are valid, optional pointers are null, and
    // ownership of the resulting descriptors is transferred to File exactly once.
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
    let mut master = unsafe { fs::File::from_raw_fd(master) };
    let slave = unsafe { fs::File::from_raw_fd(slave) };
    let slave_fd = slave.as_raw_fd();
    let mut cmd = command(temp.path());
    cmd.arg("--config")
        .arg(cfg)
        .args(["run", "--stdin", "--interactive"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(slave.try_clone().unwrap()));
    // SAFETY: The callback runs between fork and exec, uses only async-signal-safe
    // syscalls, and the inherited slave descriptor remains open until exec.
    unsafe {
        cmd.pre_exec(move || {
            if libc::setsid() < 0 || libc::ioctl(slave_fd, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().unwrap();
    drop(slave);
    child.stdin.take().unwrap().write_all(b"read it\n").unwrap();
    let mut prompt = Vec::new();
    while !String::from_utf8_lossy(&prompt).contains("[y/N]") {
        let mut poll = libc::pollfd {
            fd: master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll points to one valid pollfd and has a bounded timeout.
        if unsafe { libc::poll(&mut poll, 1, 5000) } <= 0 {
            let _ = child.kill();
            panic!("approval prompt did not arrive");
        }
        let mut buf = [0u8; 1024];
        match master.read(&mut buf) {
            Ok(0) | Err(_) => {
                let output = child.wait_with_output().unwrap();
                panic!(
                    "terminal closed before approval: {}; pty: {}",
                    String::from_utf8_lossy(&output.stderr),
                    String::from_utf8_lossy(&prompt)
                );
            }
            Ok(n) => prompt.extend_from_slice(&buf[..n]),
        }
    }
    master.write_all(b"y\n").unwrap();
    let first = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(first.to_string().contains("read it"));
    if requests.recv_timeout(Duration::from_secs(5)).is_err() {
        let _ = child.kill();
        drop(master);
        panic!("approved operation did not reach the next model request");
    }
    let mut stdout = child.stdout.take().unwrap();
    let mut poll = libc::pollfd {
        fd: stdout.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: poll observes one valid child stdout descriptor with a timeout.
    if unsafe { libc::poll(&mut poll, 1, 5000) } <= 0 {
        let _ = child.kill();
        drop(master);
        panic!("approved answer did not arrive");
    }
    let mut answer = [0u8; 128];
    let count = stdout.read(&mut answer).unwrap();
    assert_eq!(&answer[..count], b"approved\n");
    drop(stdout);
    drop(master);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&prompt).contains("read_file"));
}

#[test]
fn streamed_text_is_displayed_without_duplicate_final_answer() {
    let temp = TempDir::new().unwrap();
    let body = fixture::sse(&[
        json!({"choices":[{"index":0,"delta":{"content":"hello "},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{"content":"world"},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
    ]);
    let (url, _) = fixture::server(vec![("text/event-stream", body)]).unwrap();
    let cfg = config(temp.path(), &url, "auto");
    let source = fs::read_to_string(&cfg)
        .unwrap()
        .replace("stream = false", "stream = true");
    fs::write(&cfg, source).unwrap();
    let output = command(temp.path())
        .arg("--config")
        .arg(cfg)
        .args(["run", "greet"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "hello world\n");
}
