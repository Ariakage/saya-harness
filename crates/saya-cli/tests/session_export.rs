//! Offline acceptance through the normal CLI command, using durable fixtures.
use saya_core::{
    boundary::{Store, StoreChange},
    contract::{
        Event, EventPayload, HistoryItem, ModelProtocol, PermissionMode, Run, RunId, Session,
        SessionId, ToolCall, ToolResult, ToolState, WorkMode,
    },
    sqlite::SqliteStore,
};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};
use tempfile::TempDir;

struct Fixture {
    workspace: TempDir,
    state: TempDir,
    config: PathBuf,
    data: PathBuf,
    session: Session,
    store: Option<SqliteStore>,
}
impl Fixture {
    async fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let workspace = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        let config = state.path().join("config.toml");
        // An export can succeed without any configured execution profile.
        fs::write(&config, "schema_version = 1\n")?;
        let data = state.path().join("database");
        let call = ToolCall {
            call_id: "call-1".into(),
            name: "read_file".into(),
            arguments: json!({"path":"tool-body-marker"}).to_string(),
        };
        let result = ToolResult {
            ok: true,
            data: Some(json!({"text":"tool-body-marker"})),
            error: None,
            truncated: false,
        };
        let session = Session {
            id: SessionId::new(),
            workspace: fs::canonicalize(workspace.path())?,
            work_mode: WorkMode::Code,
            profile: "fixture".into(),
            protocol: ModelProtocol::Responses,
            model: "offline".into(),
            history: vec![
                HistoryItem::UserText("中文 🌸".into()),
                HistoryItem::AssistantText("visible <html>\u{1b}[31m\n````\n".into()),
                HistoryItem::ProviderItem {
                    protocol: ModelProtocol::Responses,
                    value: json!({"type":"reasoning","encrypted_content":"opaque-internal-marker"}),
                },
                HistoryItem::ToolCall(call.clone()),
                HistoryItem::ToolResult {
                    call_id: call.call_id.clone(),
                    result: result.clone(),
                },
            ],
        };
        let store = SqliteStore::open(&data, vec![])?;
        store.create_session(&session, "fixture-hash").await?;
        let run = Run {
            id: RunId::new(),
            session_id: session.id,
            work_mode: WorkMode::Code,
            permissions: PermissionMode::Manual,
        };
        store
            .commit(&[
                StoreChange::RunStarted(run.clone()),
                StoreChange::ToolIntent {
                    run_id: run.id,
                    call: call.clone(),
                },
                StoreChange::ToolFinished {
                    run_id: run.id,
                    call_id: call.call_id.clone(),
                    result: result.clone(),
                },
                StoreChange::ToolState {
                    run_id: run.id,
                    call_id: call.call_id.clone(),
                    state: ToolState::Unknown,
                },
                StoreChange::Event(Event {
                    schema_version: 1,
                    session_id: session.id,
                    run_id: Some(run.id),
                    seq: 1,
                    timestamp: "2026-11-08T00:00:00Z".into(),
                    payload: EventPayload::ToolFinished {
                        call_id: call.call_id,
                        name: call.name,
                        result,
                    },
                }),
            ])
            .await?;
        store.reserve_event_sequences(session.id, 100).await?;
        Ok(Self {
            workspace,
            state,
            config,
            data,
            session,
            store: Some(store),
        })
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_saya"));
        command
            .env_clear()
            .env("HOME", self.state.path())
            .env("PATH", "/usr/bin:/bin")
            .env("XDG_STATE_HOME", self.state.path().join("state"))
            .env("XDG_CACHE_HOME", self.state.path().join("cache"))
            .current_dir(self.workspace.path())
            .stdin(Stdio::null())
            .arg("--config")
            .arg(&self.config)
            .arg("--data-dir")
            .arg(&self.data);
        command
    }
    fn export(&self, format: &str, output: &str, target: &Path, flags: &[&str]) -> Output {
        self.command()
            .args([
                "--output",
                output,
                "session",
                "export",
                &self.session.id.0.to_string(),
                "--format",
                format,
                "--file",
            ])
            .arg(target)
            .args(flags)
            .output()
            .unwrap()
    }
}
fn successful(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "stderr={} stdout={}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
#[tokio::test]
async fn json_markdown_scope_and_tools_have_offline_cli_receipts()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new().await?;
    // Holding the writer lock proves export is a read-only operation.
    let _lock = fixture
        .store
        .as_ref()
        .unwrap()
        .lock_session(fixture.session.id)?;
    for (index, (format, include, scope)) in [
        ("json", false, "transcript"),
        ("json", true, "full"),
        ("markdown", false, "transcript"),
        ("markdown", true, "full"),
    ]
    .into_iter()
    .enumerate()
    {
        let target = fixture.state.path().join(format!("export-{index}"));
        let mut flags = vec!["--scope", scope];
        if include {
            flags.push("--include-tools");
        }
        let receipt = successful(&fixture.export(format, "json", &target, &flags));
        let bytes = fs::read(&target)?;
        let text = String::from_utf8(bytes.clone())?;
        assert_eq!(receipt["data"]["bytes"], bytes.len());
        assert_eq!(receipt["data"]["format"], format);
        assert_eq!(receipt["data"]["last_event_seq"], 1);
        assert_eq!(receipt["data"]["sha256"].as_str().unwrap().len(), 64);
        assert!(text.contains("中文 🌸"));
        assert_eq!(text.contains("tool-body-marker"), include);
        assert!(!text.contains("opaque-internal-marker"));
        assert!(!text.contains('\u{1b}'));
        assert_eq!(fs::metadata(&target)?.permissions().mode() & 0o777, 0o600);
        if format == "json" {
            let value: Value = serde_json::from_slice(&bytes)?;
            assert_eq!(value["export_type"], "saya.session");
            assert_eq!(value["snapshot"]["scope"], scope);
            assert_eq!(value["snapshot"]["include_tools"], include);
            assert_eq!(value["tool_calls"][0]["state"], "unknown");
            assert!(value["runs"][0]["finished_at"].is_null());
            assert_eq!(
                value["events"].as_array().unwrap().len(),
                usize::from(scope == "full")
            );
        } else {
            assert!(text.contains("&lt;html&gt;"));
            assert!(text.contains("`````text\n"));
        }
    }
    Ok(())
}
#[tokio::test]
async fn jsonl_confirmation_is_one_record_and_existing_targets_are_preserved()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new().await?;
    let target = fixture.state.path().join("session.md");
    let receipt = successful(&fixture.export("markdown", "jsonl", &target, &[]));
    assert_eq!(receipt["type"], "session.exported");
    assert!(receipt["timestamp"].is_string());
    let before = fs::read(&target)?;
    let failure = fixture.export("json", "json", &target, &[]);
    assert!(!failure.status.success());
    assert_eq!(failure.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&failure.stdout)?;
    assert_eq!(error["error"]["code"], "export_exists");
    assert_eq!(fs::read(&target)?, before);
    let failure = fixture.export("json", "jsonl", &target, &[]);
    assert_eq!(failure.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&failure.stdout)?;
    assert_eq!(error["type"], "session.export.failed");
    assert_eq!(error["error"]["code"], "export_exists");
    assert!(error["data"].is_null());
    assert!(error["timestamp"].is_string());
    let linked = fixture.state.path().join("linked.json");
    symlink(&target, &linked)?;
    let failure = fixture.export("json", "json", &linked, &[]);
    assert!(!failure.status.success());
    assert_eq!(fs::read(&target)?, before);
    assert!(fs::read_dir(fixture.state.path())?.all(|file| {
        !file
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".saya-export-")
    }));
    Ok(())
}
#[tokio::test]
async fn plan_export_checks_current_invocation_and_resolved_parent()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new().await?;
    let inside = fixture.workspace.path().join("denied.md");
    let failure = fixture.export("markdown", "json", &inside, &["--mode", "plan"]);
    assert!(!failure.status.success());
    assert!(!inside.exists());
    let alias = fixture.state.path().join("workspace-alias");
    symlink(fixture.workspace.path(), &alias)?;
    let failure = fixture.export(
        "json",
        "json",
        &alias.join("denied.json"),
        &["--mode", "plan"],
    );
    assert!(!failure.status.success());
    assert!(!fixture.workspace.path().join("denied.json").exists());
    let outside = fixture.state.path().join("allowed.md");
    successful(&fixture.export("markdown", "json", &outside, &["--mode", "plan"]));
    assert!(outside.exists());
    assert_eq!(fs::read_dir(fixture.workspace.path())?.count(), 0);
    Ok(())
}
#[tokio::test]
async fn source_errors_are_read_only_and_never_publish_a_file()
-> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = Fixture::new().await?;
    fixture.store.take();
    let db = fixture.data.join("sessions.sqlite3");
    let target = fixture.state.path().join("failure.json");
    let connection = rusqlite::Connection::open(&db)?;
    connection.pragma_update(None, "user_version", 1)?;
    drop(connection);
    let failure = fixture.export("json", "json", &target, &[]);
    assert!(!failure.status.success());
    assert!(String::from_utf8_lossy(&failure.stdout).contains("storage_migration_required"));
    assert!(!target.exists());
    let connection = rusqlite::Connection::open(&db)?;
    assert_eq!(
        connection.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))?,
        1
    );
    drop(connection);
    fs::write(&db, "invalid database")?;
    assert!(
        !fixture
            .export("json", "json", &target, &[])
            .status
            .success()
    );
    assert!(!target.exists());
    fs::remove_file(&db)?;
    let before = fs::read_dir(&fixture.data)?.count();
    assert!(
        !fixture
            .export("json", "json", &target, &[])
            .status
            .success()
    );
    assert!(!db.exists());
    assert_eq!(fs::read_dir(&fixture.data)?.count(), before);
    assert!(!target.exists());
    Ok(())
}
#[tokio::test]
async fn configured_credentials_are_redacted_without_requesting_a_model()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new().await?;
    let secret = "synthetic-export-secret";
    fs::write(
        &fixture.config,
        "schema_version = 1\n[profiles.redaction]\nprotocol = \"openai-chat-completions\"\nbase_url = \"http://127.0.0.1:9/v1\"\nmodel = \"offline\"\nauth = \"bearer\"\napi_key_env = \"EXPORT_TEST_SECRET\"\ncontext_window_tokens = 128000\n",
    )?;
    fixture
        .store
        .as_ref()
        .unwrap()
        .commit(&[StoreChange::History {
            session_id: fixture.session.id,
            item: HistoryItem::UserText(json!({"credential":secret}).to_string()),
        }])
        .await?;
    let target = fixture.state.path().join("redacted.json");
    let output = fixture
        .command()
        .env("EXPORT_TEST_SECRET", secret)
        .args([
            "--output",
            "json",
            "session",
            "export",
            &fixture.session.id.0.to_string(),
            "--format",
            "json",
            "--file",
        ])
        .arg(&target)
        .output()?;
    successful(&output);
    let text = fs::read_to_string(&target)?;
    assert!(!text.contains(secret));
    assert!(text.contains("[REDACTED]"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains(secret));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
    Ok(())
}

#[tokio::test]
async fn relative_exports_follow_explicit_workspace_and_plan_protects_saved_workspace()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new().await?;
    let current = tempfile::tempdir()?;
    let target = "relative-export.md";
    let output = fixture
        .command()
        .arg("--cwd")
        .arg(current.path())
        .args([
            "--output",
            "json",
            "session",
            "export",
            &fixture.session.id.0.to_string(),
            "--file",
            target,
        ])
        .output()?;
    successful(&output);
    assert!(current.path().join(target).is_file());
    assert!(!fixture.workspace.path().join(target).exists());
    let blocked = fixture.workspace.path().join("plan-must-not-write.md");
    let output = fixture
        .command()
        .arg("--cwd")
        .arg(current.path())
        .args([
            "--mode",
            "plan",
            "--output",
            "json",
            "session",
            "export",
            &fixture.session.id.0.to_string(),
            "--file",
        ])
        .arg(&blocked)
        .output()?;
    assert_eq!(output.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(error["error"]["code"], "plan_write_denied");
    assert!(!blocked.exists());
    Ok(())
}
