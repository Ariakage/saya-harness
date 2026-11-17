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
fn saved_session_can_be_listed_shown_and_used_for_a_new_task() {
    let temp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let (url, requests) = fixture::server(vec![
        (
            "application/json",
            response(json!({"role":"assistant","content":"first reply"}), "stop"),
        ),
        (
            "application/json",
            response(json!({"role":"assistant","content":"second reply"}), "stop"),
        ),
    ])
    .unwrap();
    let cfg = config(temp.path(), &url, "auto");
    let first = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["run", "first question"])
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let list = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["--output", "json", "session", "list"])
        .output()
        .unwrap();
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    let list: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let id = list["data"][0]["id"].as_str().unwrap();
    assert_eq!(list["data"][0]["title"], "first question");
    let show = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["--output", "json", "session", "show", id])
        .output()
        .unwrap();
    assert!(
        show.status.success(),
        "{}",
        String::from_utf8_lossy(&show.stderr)
    );
    assert!(String::from_utf8_lossy(&show.stdout).contains("first reply"));
    let second = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["run", "--session", id, "second question"])
        .output()
        .unwrap();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let _ = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    let request = requests
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .to_string();
    assert!(request.contains("first question"));
    assert!(request.contains("first reply"));
    assert!(request.contains("second question"));
}

#[test]
fn explicit_plan_override_on_resumed_run_prevents_writes_and_is_saved() {
    for permissions in ["auto", "manual", "bypass"] {
        let workspace = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        let external = TempDir::new().unwrap();
        let (url, requests) = fixture::server(vec![
            (
                "application/json",
                response(json!({"role":"assistant","content":"started"}), "stop"),
            ),
            (
                "application/json",
                tool_response(
                    "write",
                    "write_file",
                    json!({"path":"forbidden.txt","content":"changed","expected_version":null}),
                ),
            ),
            (
                "application/json",
                response(json!({"role":"assistant","content":"plan ready"}), "stop"),
            ),
        ])
        .unwrap();
        let cfg = config(workspace.path(), &url, permissions);
        let mut first = command(workspace.path(), home.path());
        first
            .arg("--config")
            .arg(&cfg)
            .args(["--output", "json", "run", "start"]);
        for name in ["XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME"] {
            first.env(name, external.path().join(name));
        }
        let first = first.output().unwrap();
        assert!(
            first.status.success(),
            "{}",
            String::from_utf8_lossy(&first.stderr)
        );
        let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
        let id = first["session_id"].as_str().unwrap();
        let mut resumed = command(workspace.path(), home.path());
        resumed.arg("--config").arg(&cfg).args([
            "--mode",
            "plan",
            "--output",
            "json",
            "run",
            "--session",
            id,
            "inspect",
        ]);
        for name in ["XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME"] {
            resumed.env(name, external.path().join(name));
        }
        let resumed = resumed.output().unwrap();
        assert!(
            !workspace.path().join("forbidden.txt").exists(),
            "{permissions} resumed with a writable mode"
        );
        assert!(
            resumed.status.success(),
            "{permissions}: {}",
            String::from_utf8_lossy(&resumed.stderr)
        );
        requests.recv_timeout(Duration::from_secs(2)).unwrap();
        let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
        let context: serde_json::Value =
            serde_json::from_str(request["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(context["saya_runtime_context_v1"]["work_mode"], "plan");
        let mut show = command(workspace.path(), home.path());
        show.arg("--config")
            .arg(&cfg)
            .args(["--output", "json", "session", "show", id])
            .env("XDG_DATA_HOME", external.path().join("XDG_DATA_HOME"));
        let show = show.output().unwrap();
        assert!(show.status.success());
        let show: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
        assert_eq!(show["data"]["work_mode"], "plan");
    }
}

#[test]
fn configured_write_limit_reaches_the_real_dispatcher() {
    let temp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let (url, requests) = fixture::server(vec![
        (
            "application/json",
            tool_response(
                "write",
                "write_file",
                json!({"path":"large.txt","content":"123456789","expected_version":null}),
            ),
        ),
        (
            "application/json",
            response(json!({"role":"assistant","content":"done"}), "stop"),
        ),
    ])
    .unwrap();
    let cfg = config(temp.path(), &url, "auto");
    fs::write(
        &cfg,
        format!(
            "{}\n[limits]\nwrite_max_bytes = 8\n",
            fs::read_to_string(&cfg).unwrap()
        ),
    )
    .unwrap();
    let result = command(temp.path(), home.path())
        .arg("--config")
        .arg(cfg)
        .args(["run", "write"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!temp.path().join("large.txt").exists());
    requests.recv_timeout(Duration::from_secs(2)).unwrap();
    let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(request.to_string().contains("write_too_large"));
}

#[test]
fn resume_requires_explicit_acceptance_for_profile_changes() {
    let temp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let (url, _) = fixture::server(vec![
        (
            "application/json",
            response(json!({"role":"assistant","content":"one"}), "stop"),
        ),
        (
            "application/json",
            response(json!({"role":"assistant","content":"two"}), "stop"),
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
    let old = fs::read_to_string(&cfg).unwrap();
    fs::write(
        &cfg,
        old.replace("stream = false", "stream = false\nmax_output_tokens = 1024"),
    )
    .unwrap();
    let rejected = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["run", "--session", id, "continue"])
        .output()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("--accept-config-change"));
    let accepted = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["run", "--session", id, "--accept-config-change", "continue"])
        .output()
        .unwrap();
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    let current = fs::read_to_string(&cfg).unwrap();
    fs::write(
        &cfg,
        current.replace("model = \"fake\"", "model = \"different\""),
    )
    .unwrap();
    let incompatible = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["run", "--session", id, "--accept-config-change", "continue"])
        .output()
        .unwrap();
    assert_eq!(incompatible.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&incompatible.stderr).contains("start a new session"));
}

#[test]
fn resuming_plan_checks_workspace_before_creating_sqlite_side_files() {
    use saya_core::{
        boundary::{Store, StoreChange},
        contract::{ModelProtocol, Session, SessionId, WorkMode},
        sqlite::SqliteStore,
    };
    for saved_mode in [WorkMode::Plan, WorkMode::Work] {
        let temp = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let data = root.join(".local/share/saya");
        let id = SessionId::new();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        runtime.block_on(async {
            let store = SqliteStore::open(&data, vec![]).unwrap();
            let session = Session {
                id,
                workspace: root.clone(),
                work_mode: saved_mode,
                profile: "test".into(),
                protocol: ModelProtocol::ChatCompletions,
                model: "fake".into(),
                history: vec![],
            };
            store
                .commit(&[StoreChange::Session(session)])
                .await
                .unwrap();
        });
        let db = data.join("sessions.sqlite3");
        let before = fs::read(&db).unwrap();
        let before_names: std::collections::BTreeSet<_> = fs::read_dir(&data)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        let cfg = config(temp.path(), "http://127.0.0.1:1/v1", "auto");
        let result = command(temp.path(), home.path())
            .env("XDG_DATA_HOME", root.join(".local/share"))
            .arg("--config")
            .arg(cfg)
            .args([
                "--mode",
                "plan",
                "run",
                "--session",
                &id.0.to_string(),
                "inspect",
            ])
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&result.stderr).contains("PLAN storage directories overlap")
        );
        assert_eq!(fs::read(db).unwrap(), before);
        let after_names: std::collections::BTreeSet<_> = fs::read_dir(data)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(before_names, after_names);
    }
}

#[test]
fn new_plan_rejects_each_overlapping_storage_root_without_workspace_changes() {
    for selected in ["XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME"] {
        let workspace = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        let external = TempDir::new().unwrap();
        let cfg = config(workspace.path(), "http://127.0.0.1:1/v1", "auto");
        let before: std::collections::BTreeSet<_> = fs::read_dir(workspace.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        let mut cmd = command(workspace.path(), home.path());
        cmd.arg("--config")
            .arg(&cfg)
            .args(["--mode", "plan", "run", "inspect"]);
        for name in ["XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME"] {
            let root = if name == selected {
                workspace.path().join("inside")
            } else {
                external.path().join(name)
            };
            cmd.env(name, root);
        }
        let result = cmd.output().unwrap();
        assert_eq!(result.status.code(), Some(2), "{selected}");
        assert!(
            String::from_utf8_lossy(&result.stderr).contains("PLAN storage directories overlap")
        );
        let after: std::collections::BTreeSet<_> = fs::read_dir(workspace.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(before, after, "{selected}");
        assert!(!external.path().join("XDG_DATA_HOME/saya").exists());
    }
}

#[test]
fn plan_rejects_home_workspace_and_symlink_overlap_without_creating_files() {
    use std::os::unix::fs::symlink;
    let workspace = TempDir::new().unwrap();
    let alias_root = TempDir::new().unwrap();
    let alias = alias_root.path().join("home");
    symlink(workspace.path(), &alias).unwrap();
    let cfg = config(workspace.path(), "http://127.0.0.1:1/v1", "auto");
    let before: std::collections::BTreeSet<_> = fs::read_dir(workspace.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    for home in [workspace.path(), alias.as_path()] {
        let result = command(workspace.path(), home)
            .arg("--config")
            .arg(&cfg)
            .args(["--mode", "plan", "run", "inspect"])
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(2), "{home:?}");
        assert!(
            String::from_utf8_lossy(&result.stderr).contains("PLAN storage directories overlap")
        );
    }
    let after: std::collections::BTreeSet<_> = fs::read_dir(workspace.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(before, after);
}

#[test]
fn home_workspace_with_external_xdg_runs_and_protects_existing_memory() {
    let workspace = TempDir::new().unwrap();
    let config_root = TempDir::new().unwrap();
    let storage = TempDir::new().unwrap();
    let memory_root = workspace.path().join(".saya");
    fs::create_dir_all(memory_root.join("memory")).unwrap();
    let memory = saya_core::global_memory::GlobalMemoryStore::open(
        &memory_root,
        saya_core::global_memory::WorkspaceIdentity::from_path(workspace.path()).unwrap(),
        saya_core::global_memory::MemoryLimits::default(),
        std::sync::Arc::new(saya_core::credentials::SecretRegistry::default()),
    )
    .unwrap();
    fs::write(
        memory_root.join("MEMORY.md"),
        memory
            .render_document(&saya_core::global_memory::MemoryDocument::default())
            .unwrap(),
    )
    .unwrap();
    fs::write(memory_root.join(".saya-memory.lock"), "").unwrap();
    let read = tool_response(
        "read-memory",
        "read_file",
        json!({"path":".saya/MEMORY.md","start_line":1,"line_count":10}),
    );
    let (url, requests) = fixture::server(vec![
        ("application/json", read),
        (
            "application/json",
            response(json!({"role":"assistant","content":"done"}), "stop"),
        ),
    ])
    .unwrap();
    let cfg = config(config_root.path(), &url, "bypass");
    let result = command(workspace.path(), workspace.path())
        .env("XDG_DATA_HOME", storage.path().join("data"))
        .env("XDG_STATE_HOME", storage.path().join("state"))
        .env("XDG_CACHE_HOME", storage.path().join("cache"))
        .arg("--config")
        .arg(cfg)
        .args(["run", "inspect protected memory"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let requests = requests.try_iter().collect::<Vec<_>>();
    assert_eq!(requests.len(), 2);
    let tool_messages = requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .collect::<Vec<_>>();
    assert!(
        tool_messages
            .iter()
            .filter_map(|message| message["content"].as_str())
            .filter_map(|content| serde_json::from_str::<serde_json::Value>(content).ok())
            .any(|result| result["ok"] == false),
        "{tool_messages:?}"
    );
}
