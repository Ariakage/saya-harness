use crate::common::*;
use crate::fixture;
use serde_json::json;
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};
use tempfile::TempDir;

#[test]
fn cli_session_lock_child() -> Result<(), Box<dyn std::error::Error>> {
    use saya_core::{
        boundary::{Store, StoreChange},
        contract::{HistoryItem, SessionId},
        sqlite::SqliteStore,
    };
    let Ok(directory) = std::env::var("SAYA_TEST_LOCK_DIR") else {
        return Ok(());
    };
    let id: SessionId = serde_json::from_value(json!(std::env::var("SAYA_TEST_LOCK_ID")?))?;
    let store = SqliteStore::open(Path::new(&directory), vec![])?;
    let _lock = SqliteStore::lock_existing(Path::new(&directory), id)?;
    let runtime = tokio::runtime::Builder::new_current_thread().build()?;
    runtime.block_on(store.commit(&[StoreChange::History {
        session_id: id,
        item: HistoryItem::UserText("held session".into()),
    }]))?;
    println!("SAYA_CHILD_READY");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(())
}

#[test]
fn busy_session_does_not_prevent_resuming_another_session() -> Result<(), Box<dyn std::error::Error>>
{
    use std::{
        io::{BufRead, BufReader},
        sync::mpsc,
    };
    let temp = TempDir::new()?;
    let home = TempDir::new()?;
    let (url, _) = fixture::server(vec![
        (
            "application/json",
            response(json!({"role":"assistant","content":"one"}), "stop"),
        ),
        (
            "application/json",
            response(json!({"role":"assistant","content":"two"}), "stop"),
        ),
        (
            "application/json",
            response(json!({"role":"assistant","content":"continued"}), "stop"),
        ),
    ])?;
    let cfg = config(temp.path(), &url, "auto");
    for prompt in ["first", "second"] {
        let output = command(temp.path(), home.path())
            .arg("--config")
            .arg(&cfg)
            .args(["run", prompt])
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let listing = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["--output", "json", "session", "list"])
        .output()?;
    let data: serde_json::Value = serde_json::from_slice(&listing.stdout)?;
    let entries = data["data"].as_array().ok_or("missing entries")?;
    let get_id = |title: &str| {
        entries
            .iter()
            .find(|entry| entry["title"] == title)
            .and_then(|entry| entry["id"].as_str())
            .map(str::to_owned)
            .ok_or("missing session")
    };
    let held = get_id("first")?;
    let other = get_id("second")?;
    let mut child = Command::new(std::env::current_exe()?)
        .args(["--exact", "recovery::cli_session_lock_child", "--nocapture"])
        .env("SAYA_TEST_LOCK_DIR", home.path().join(".local/share/saya"))
        .env("SAYA_TEST_LOCK_ID", &held)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().ok_or("missing stdout")?;
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if line.contains("SAYA_CHILD_READY") {
                let _ = send.send(());
            }
        }
    });
    receive.recv_timeout(Duration::from_secs(5))?;
    let busy = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["run", "--session", &held, "retry"])
        .output()?;
    assert!(!busy.status.success());
    assert!(
        String::from_utf8_lossy(&busy.stderr).contains("session_busy"),
        "{}",
        String::from_utf8_lossy(&busy.stderr)
    );
    // B-17: machine output must preserve the real error code and use exit 1
    // for runtime failures rather than the generic invalid_input/2.
    let busy_json = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["--output", "json", "run", "--session", &held, "retry"])
        .output()?;
    assert_eq!(busy_json.status.code(), Some(1));
    let result: serde_json::Value = serde_json::from_slice(&busy_json.stdout)?;
    assert_eq!(result["status"], "failed");
    assert_eq!(result["stop_reason"], "session_busy");
    assert_eq!(result["error"]["code"], "session_busy");
    let busy_jsonl = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["--output", "jsonl", "run", "--session", &held, "retry"])
        .output()?;
    assert_eq!(busy_jsonl.status.code(), Some(1));
    let final_event: serde_json::Value = String::from_utf8(busy_jsonl.stdout)?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .find(|event: &serde_json::Value| event["type"] == "run.finished")
        .ok_or("missing run.finished")?;
    assert_eq!(final_event["data"]["error"]["code"], "session_busy");
    let available = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["run", "--session", &other, "continue"])
        .output()?;
    assert!(
        available.status.success(),
        "{}",
        String::from_utf8_lossy(&available.stderr)
    );
    child
        .stdin
        .take()
        .ok_or("missing stdin")?
        .write_all(b"exit\n")?;
    assert!(child.wait()?.success());
    Ok(())
}

#[test]
fn cli_crash_child() -> Result<(), Box<dyn std::error::Error>> {
    use saya_core::{
        boundary::{Store, StoreChange},
        contract::{HistoryItem, PermissionMode, Run, RunId, SessionId, ToolCall},
        sqlite::SqliteStore,
    };
    let Ok(directory) = std::env::var("SAYA_TEST_CRASH_DIR") else {
        return Ok(());
    };
    let id: SessionId = serde_json::from_value(json!(std::env::var("SAYA_TEST_CRASH_ID")?))?;
    let store = SqliteStore::open(Path::new(&directory), vec![])?;
    let _lock = SqliteStore::lock_existing(Path::new(&directory), id)?;
    let runtime = tokio::runtime::Builder::new_current_thread().build()?;
    let session = runtime
        .block_on(store.load_session(id))?
        .ok_or("missing session")?;
    let run = Run {
        id: RunId::new(),
        session_id: id,
        work_mode: session.work_mode,
        permissions: PermissionMode::Auto,
    };
    let call = ToolCall {
        call_id: "crashed-call".into(),
        name: "exec".into(),
        arguments: json!({"argv":["touch",std::env::var("SAYA_TEST_CRASH_TARGET")?]}).to_string(),
    };
    runtime.block_on(store.commit(&[
        StoreChange::RunStarted(run.clone()),
        StoreChange::History {
            session_id: id,
            item: HistoryItem::ToolCall(call.clone()),
        },
        StoreChange::ToolIntent {
            run_id: run.id,
            call,
        },
    ]))?;
    println!("SAYA_CRASH_READY");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(())
}

#[test]
fn crashed_run_is_recovered_with_unknown_result_without_replay()
-> Result<(), Box<dyn std::error::Error>> {
    exercise_crash_recovery(false)
}

#[test]
fn legacy_session_recovers_stranded_wal_without_writing_during_preflight()
-> Result<(), Box<dyn std::error::Error>> {
    exercise_crash_recovery(true)
}

fn exercise_crash_recovery(legacy: bool) -> Result<(), Box<dyn std::error::Error>> {
    use std::{
        io::{BufRead, BufReader},
        sync::mpsc,
    };
    let temp = TempDir::new()?;
    let home = TempDir::new()?;
    let (url, requests) = fixture::server(vec![
        (
            "application/json",
            response(json!({"role":"assistant","content":"first"}), "stop"),
        ),
        (
            "application/json",
            response(json!({"role":"assistant","content":"recovered"}), "stop"),
        ),
    ])?;
    let cfg = config(temp.path(), &url, "auto");
    let output = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["run", "initial"])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let listing = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["--output", "json", "session", "list"])
        .output()?;
    let listing: serde_json::Value = serde_json::from_slice(&listing.stdout)?;
    let id = listing["data"][0]["id"].as_str().ok_or("missing ID")?;
    let target = temp.path().join("must-not-exist");
    let mut child = Command::new(std::env::current_exe()?)
        .args(["--exact", "recovery::cli_crash_child", "--nocapture"])
        .env("SAYA_TEST_CRASH_DIR", home.path().join(".local/share/saya"))
        .env("SAYA_TEST_CRASH_ID", id)
        .env("SAYA_TEST_CRASH_TARGET", &target)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let (send, receive) = mpsc::channel();
    let stdout = child.stdout.take().ok_or("missing stdout")?;
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if line.contains("SAYA_CRASH_READY") {
                let _ = send.send(());
            }
        }
    });
    receive.recv_timeout(Duration::from_secs(5))?;
    child.kill()?;
    child.wait()?;
    if legacy {
        let data = home.path().join(".local/share/saya");
        fs::remove_file(data.join("locks").join(format!("{id}.guard")))?;
        let files = [
            "sessions.sqlite3",
            "sessions.sqlite3-wal",
            "sessions.sqlite3-shm",
        ];
        let before = files.map(|name| fs::read(data.join(name)).unwrap());
        let session_id: saya_core::contract::SessionId = serde_json::from_value(json!(id))?;
        assert!(saya_core::sqlite::SqliteStore::peek_session(&data, session_id)?.is_some());
        let after = files.map(|name| fs::read(data.join(name)).unwrap());
        assert_eq!(before, after);
    }
    let resumed = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["run", "--session", id, "continue"])
        .output()?;
    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    assert!(String::from_utf8_lossy(&resumed.stderr).contains("1 tool outcome(s) unknown"));
    assert!(!target.exists());
    let _ = requests.recv_timeout(Duration::from_secs(2))?;
    let request = requests.recv_timeout(Duration::from_secs(2))?.to_string();
    assert!(request.contains("crashed-call"));
    assert!(request.contains("outcome_unknown"));
    Ok(())
}

#[test]
fn observed_tool_result_is_reported_when_persistence_fails()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let home = TempDir::new()?;
    fs::write(temp.path().join("input.txt"), "observed fixture data")?;
    let (url, _) = fixture::server(vec![
        (
            "application/json",
            response(json!({"role":"assistant","content":"first"}), "stop"),
        ),
        ("application/json", read_call()),
    ])?;
    let cfg = config(temp.path(), &url, "auto");
    assert!(
        command(temp.path(), home.path())
            .arg("--config")
            .arg(&cfg)
            .args(["run", "first"])
            .output()?
            .status
            .success()
    );
    let list = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["--output", "json", "session", "list"])
        .output()?;
    let list: serde_json::Value = serde_json::from_slice(&list.stdout)?;
    let id = list["data"][0]["id"].as_str().ok_or("missing id")?;
    let db = rusqlite::Connection::open(home.path().join(".local/share/saya/sessions.sqlite3"))?;
    db.execute_batch("CREATE TRIGGER reject_tool_result BEFORE UPDATE OF result ON tool_calls BEGIN SELECT RAISE(ABORT,'test failure'); END;")?;
    let output = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["run", "--session", id, "read"])
        .output()?;
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("observed tool result (not saved)"),
        "{stderr}"
    );
    assert!(stderr.contains("observed fixture data"), "{stderr}");
    Ok(())
}

#[test]
fn observed_result_redacts_configured_secret_when_save_is_rejected()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let home = TempDir::new()?;
    let secret = "fake-credential-for-test";
    fs::write(
        temp.path().join("input.txt"),
        format!("prefix {secret} suffix"),
    )?;
    let (url, _) = fixture::server(vec![
        (
            "application/json",
            response(json!({"role":"assistant","content":"first"}), "stop"),
        ),
        ("application/json", read_call()),
    ])?;
    let cfg = config(temp.path(), &url, "auto");
    let contents = fs::read_to_string(&cfg)?.replace(
        "auth = \"none\"",
        "auth = \"bearer\"\napi_key_env = \"TEST_KEY\"",
    );
    fs::write(&cfg, contents)?;
    let first = command(temp.path(), home.path())
        .env("TEST_KEY", secret)
        .arg("--config")
        .arg(&cfg)
        .args(["run", "start"])
        .output()?;
    assert!(first.status.success());
    let list = command(temp.path(), home.path())
        .env("TEST_KEY", secret)
        .arg("--config")
        .arg(&cfg)
        .args(["--output", "json", "session", "list"])
        .output()?;
    let list: serde_json::Value = serde_json::from_slice(&list.stdout)?;
    let id = list["data"][0]["id"].as_str().ok_or("missing session")?;
    let db = rusqlite::Connection::open(home.path().join(".local/share/saya/sessions.sqlite3"))?;
    db.execute_batch("CREATE TRIGGER reject_result BEFORE UPDATE OF result ON tool_calls BEGIN SELECT RAISE(ABORT,'test failure'); END;")?;
    let output = command(temp.path(), home.path())
        .env("TEST_KEY", secret)
        .arg("--config")
        .arg(cfg)
        .args(["run", "--session", id, "read"])
        .output()?;
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("observed tool result (not saved)"),
        "{stderr}"
    );
    assert!(stderr.contains("[REDACTED]"), "{stderr}");
    assert!(!stderr.contains(secret));
    Ok(())
}
