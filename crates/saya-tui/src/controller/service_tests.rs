//! Explicit UI operations preserve durable session and publication outcomes.
use super::services::{UiJob, UiResult};
use super::*;
use saya_core::{
    boundary::{Cancellation, StoreChange},
    config::{Environment, Overrides},
    contract::{ErrorInfo, HistoryItem, PermissionMode, Run, ToolCall, ToolResult, ToolState},
    sqlite::{SnapshotLimits, SqliteStore},
};
use std::{
    ffi::OsString,
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

fn fixture() -> (tempfile::TempDir, App) {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let path = root.path().join("config.toml");
    std::fs::write(
        &path,
        r#"schema_version = 1
[defaults]
profile = "fixture"
[profiles.fixture]
protocol = "openai-chat-completions"
base_url = "http://127.0.0.1:9/v1"
model = "fixture"
auth = "none"
context_window_tokens = 128000
stream = false
"#,
    )
    .unwrap();
    let config = saya_core::config::load(
        Overrides {
            config: Some(path),
            ..Default::default()
        },
        Environment::from_pairs([(OsString::from("HOME"), root.path().as_os_str().to_owned())]),
        &workspace,
    )
    .unwrap();
    let app = App::new(
        config,
        Options {
            personal_context: PersonalContext::default(),
            anthropic_prompt_cache: false,
            cwd_explicit: false,
            profile_explicit: false,
            mode_override: None,
        },
    )
    .unwrap();
    (root, app)
}
async fn outcome(app: &mut App) -> std::result::Result<UiResult, BoundaryError> {
    tokio::time::timeout(Duration::from_secs(8), services::next_job(&mut app.ui_job))
        .await
        .unwrap()
}
async fn complete(app: &mut App) {
    let result = match outcome(app).await {
        Ok(result) => result,
        Err(error) => panic!("UI operation failed: {error}"),
    };
    app.finish_ui(Ok(result)).await.unwrap();
}
async fn create(app: &mut App) {
    app.service("/new", "").await.unwrap();
    complete(app).await;
}
fn controlled_job(app: &mut App) -> (oneshot::Sender<UiResult>, Arc<RunCancellation>) {
    let (sender, receiver) = oneshot::channel();
    let cancel = Arc::new(RunCancellation::new());
    let join = tokio::spawn(async move {
        receiver.await.map_err(|_| {
            BoundaryError(
                ErrorInfo {
                    code: "test_worker_closed".into(),
                    message: "test worker closed".into(),
                },
                None,
                None,
            )
        })
    });
    app.ui_job = Some(UiJob {
        cancel: cancel.clone(),
        join,
    });
    (sender, cancel)
}
fn transcript(app: &App) -> String {
    app.state
        .messages
        .iter()
        .map(|message| message.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}
async fn snapshot(app: &App) -> saya_core::sqlite::SessionSnapshot {
    SqliteStore::read_snapshot(
        &app.config.data_dir,
        app.prepared.as_ref().unwrap().session.id,
        SnapshotLimits::default(),
        Arc::new(RunCancellation::new()),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn new_and_resume_wait_for_input_and_hold_only_the_open_session_lock() {
    let (_root, mut app) = fixture();
    create(&mut app).await;
    let mut prepared = app.prepared.take().unwrap();
    let id = prepared.session.id;
    let store = prepared.store.clone();
    session::append(&mut prepared, "saved question".into())
        .await
        .unwrap();
    assert_eq!(store.lock_session(id).err().unwrap().0.code, "session_busy");
    drop(prepared);
    let released = SqliteStore::lock_existing(&app.config.data_dir, id).unwrap();
    drop(released);
    app.service("/resume", &id.0.to_string()).await.unwrap();
    complete(&mut app).await;
    assert!(app.active.is_none());
    assert_eq!(app.state.status, "Ready");
    assert_eq!(
        app.prepared.as_ref().unwrap().session.history,
        vec![HistoryItem::UserText("saved question".into())]
    );
    assert!(transcript(&app).contains("saved question"));
    assert!(snapshot(&app).await.runs.is_empty());
    assert_eq!(store.lock_session(id).err().unwrap().0.code, "session_busy");
    let data = app.config.data_dir.clone();
    drop(app);
    let _released = SqliteStore::lock_existing(&data, id).unwrap();
}

#[tokio::test]
async fn configuration_changes_need_explicit_resume_accept_and_failed_resume_releases_lock() {
    let (_root, mut app) = fixture();
    create(&mut app).await;
    let prepared = app.prepared.take().unwrap();
    let id = prepared.session.id;
    let store = prepared.store.clone();
    let original = store.fingerprint(id).await.unwrap();
    drop(prepared);
    app.config
        .profiles
        .get_mut("fixture")
        .unwrap()
        .max_output_tokens += 1;
    app.service("/resume", &id.0.to_string()).await.unwrap();
    let error = outcome(&mut app).await.err().unwrap();
    assert!(error.0.message.contains("configuration changed"));
    app.finish_ui(Err(error)).await.unwrap();
    assert!(app.prepared.is_none());
    assert_eq!(store.fingerprint(id).await.unwrap(), original);
    let released = SqliteStore::lock_existing(&app.config.data_dir, id).unwrap();
    drop(released);
    app.service("/resume-accept", &id.0.to_string())
        .await
        .unwrap();
    complete(&mut app).await;
    assert_eq!(app.prepared.as_ref().unwrap().session.id, id);
    assert!(app.active.is_none());
    assert_eq!(
        store.fingerprint(id).await.unwrap(),
        app.config.profiles["fixture"].fingerprint().unwrap()
    );
}

#[tokio::test]
async fn ui_queries_preserve_unknown_truncation_and_run_call_links_without_agent_tools() {
    let (_root, mut app) = fixture();
    create(&mut app).await;
    let prepared = app.prepared.as_ref().unwrap();
    let id = prepared.session.id;
    let store = prepared.store.clone();
    let mut ids = Vec::new();
    for (index, state) in [ToolState::Unknown, ToolState::Failed]
        .into_iter()
        .enumerate()
    {
        let run = Run {
            id: RunId::new(),
            session_id: id,
            work_mode: WorkMode::Work,
            permissions: PermissionMode::Manual,
        };
        ids.push(run.id);
        let call = ToolCall {
            call_id: "reused-call".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path":format!("fixture-{index}")}).to_string(),
        };
        store
            .commit(&[
                StoreChange::RunStarted(run.clone()),
                StoreChange::ToolIntent {
                    run_id: run.id,
                    call: call.clone(),
                },
            ])
            .await
            .unwrap();
        if index == 1 {
            store
                .commit(&[StoreChange::ToolFinished {
                    run_id: run.id,
                    call_id: call.call_id.clone(),
                    result: ToolResult {
                        ok: false,
                        data: Some(serde_json::json!({"text":"retained prefix"})),
                        error: Some(ErrorInfo {
                            code: "truncated_fixture".into(),
                            message: "bounded output".into(),
                        }),
                        truncated: true,
                    },
                }])
                .await
                .unwrap();
        } else {
            store
                .commit(&[StoreChange::ToolState {
                    run_id: run.id,
                    call_id: call.call_id,
                    state,
                }])
                .await
                .unwrap();
        }
    }
    app.config.permissions_mode = PermissionMode::Manual;
    let before = snapshot(&app).await;
    for command in ["/sessions", "/tools", "/jobs"] {
        app.service(command, "").await.unwrap();
        complete(&mut app).await;
        assert!(app.pending.is_none());
        assert!(app.state.details.as_ref().unwrap().title.contains("UI"));
        if command == "/tools" {
            let text = app.state.details.as_ref().unwrap().lines.join("\n");
            assert!(text.contains("unknown"));
            assert!(text.contains("\"truncated\": true"));
            assert!(text.contains("reused-call"));
            for run in &ids {
                assert!(text.contains(&run.0.to_string()));
            }
        }
    }
    let after = snapshot(&app).await;
    assert_eq!(before.last_change_seq, after.last_change_seq);
    assert_eq!(before.runs, after.runs);
    assert_eq!(before.tool_calls, after.tool_calls);
    assert!(after.approval_requests.is_empty());
    assert!(app.active.is_none());
}

#[tokio::test]
async fn cancelling_ui_work_discards_deferred_mode_and_blocks_permission_changes_until_finished() {
    let (_root, mut app) = fixture();
    let (sender, cancel) = controlled_job(&mut app);
    assert!(app.command("/permissions bypass").await.is_err());
    assert_eq!(app.config.permissions_mode, PermissionMode::Auto);
    app.command("/mode plan").await.unwrap();
    assert_eq!(app.config.mode, WorkMode::Work);
    assert_eq!(app.state.pending_mode.as_deref(), Some("PLAN"));
    assert!(cancel.is_cancelled());
    app.cancel();
    app.refresh_mode().unwrap();
    assert!(app.deferred_mode.is_none());
    assert!(app.state.pending_mode.is_none());
    assert!(
        sender
            .send(UiResult::Details(crate::view::Details {
                title: "stale".into(),
                lines: vec!["stale data".into()]
            }))
            .is_ok()
    );
    complete(&mut app).await;
    assert!(app.state.details.is_none());
    assert!(!transcript(&app).contains("stale data"));
}

#[tokio::test]
async fn a_published_export_is_reported_after_cancel_before_the_deferred_mode_is_applied() {
    let (root, mut app) = fixture();
    create(&mut app).await;
    let target = root.path().join("published.json");
    let receipt = saya_app::export::publish(
        &target,
        b"{}",
        &app.config.cwd,
        WorkMode::Work,
        &RunCancellation::new(),
    )
    .unwrap();
    let (sender, cancel) = controlled_job(&mut app);
    app.command("/mode plan").await.unwrap();
    assert!(cancel.is_cancelled());
    assert_eq!(app.config.mode, WorkMode::Work);
    assert!(sender.send(UiResult::Export(receipt)).is_ok());
    complete(&mut app).await;
    app.refresh_mode().unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"{}");
    assert!(transcript(&app).contains("Exported"));
    assert!(transcript(&app).contains("published.json"));
    assert_eq!(app.config.mode, WorkMode::Plan);
    assert!(app.state.pending_mode.is_none());
}

#[tokio::test]
async fn cancelled_session_activation_keeps_current_session_and_releases_candidate_lock() {
    let (_root, mut app) = fixture();
    create(&mut app).await;
    let original = app.prepared.as_ref().unwrap().session.id;
    let candidate = session::create(&app.config, saya_app::new_session(&app.config).unwrap())
        .await
        .unwrap();
    let candidate_id = candidate.session.id;
    let (sender, _) = controlled_job(&mut app);
    app.cancel();
    assert!(
        sender
            .send(UiResult::Session {
                prepared: candidate,
                resumed: false
            })
            .is_ok()
    );
    complete(&mut app).await;
    assert_eq!(app.prepared.as_ref().unwrap().session.id, original);
    let _released = SqliteStore::lock_existing(&app.config.data_dir, candidate_id).unwrap();
}

#[tokio::test]
async fn incomplete_export_commands_preserve_the_draft_without_starting_a_worker() {
    let (_root, mut app) = fixture();
    create(&mut app).await;
    let before = snapshot(&app).await;
    for text in [
        "/export json ",
        "/export-full markdown ",
        "/export unsupported target",
    ] {
        app.state.editor.set_text(text);
        let error = app
            .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<BoundaryError>().unwrap().0.code,
            "invalid_input"
        );
        assert_eq!(app.state.editor.text(), text);
        assert!(app.ui_job.is_none() && app.active.is_none());
    }
    assert_eq!(before.last_change_seq, snapshot(&app).await.last_change_seq);
}

#[tokio::test]
async fn ui_export_uses_current_plan_mode_and_preserves_existing_files() {
    let (root, mut app) = fixture();
    create(&mut app).await;
    app.command("/mode plan").await.unwrap();
    app.service("/export", "json inside.json").await.unwrap();
    let error = outcome(&mut app).await.err().unwrap();
    assert_eq!(error.0.code, "plan_write_denied");
    app.finish_ui(Err(error)).await.unwrap();
    assert!(!app.config.cwd.join("inside.json").exists());
    let outside = root.path().join("outside.json");
    app.service("/export-full", &format!("json {}", outside.display()))
        .await
        .unwrap();
    complete(&mut app).await;
    let bytes = std::fs::read(&outside).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["snapshot"]["scope"], "full");
    assert_eq!(value["snapshot"]["include_tools"], true);
    app.service("/export", &format!("markdown {}", outside.display()))
        .await
        .unwrap();
    let error = outcome(&mut app).await.err().unwrap();
    assert_eq!(error.0.code, "export_exists");
    app.finish_ui(Err(error)).await.unwrap();
    assert_eq!(std::fs::read(&outside).unwrap(), bytes);
    assert!(snapshot(&app).await.runs.is_empty());
}

#[tokio::test]
async fn exiting_waits_for_ui_completion_and_retains_the_actual_export_receipt() {
    for keyboard in [false, true] {
        let (_root, mut app) = fixture();
        let (sender, cancel) = controlled_job(&mut app);
        let exit = if keyboard {
            app.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))
                .await
                .unwrap()
        } else {
            app.command("/exit").await.unwrap()
        };
        assert!(!exit);
        assert_eq!(app.exit_after_run, Some(0));
        assert!(cancel.is_cancelled());
        assert!(
            sender
                .send(UiResult::Export(
                    serde_json::json!({"path":"published.json"})
                ))
                .is_ok()
        );
        complete(&mut app).await;
        assert!(app.ui_job.is_none());
        assert!(transcript(&app).contains("Exported"));
        assert!(transcript(&app).contains("published.json"));
    }
}

#[tokio::test]
async fn query_metadata_is_redacted_before_entering_the_ui_cache() {
    let (_root, mut app) = fixture();
    create(&mut app).await;
    let id = app.prepared.as_ref().unwrap().session.id.0.to_string();
    app.secrets.push(id.clone());
    for command in ["/tools", "/jobs", "/sessions"] {
        app.service(command, "").await.unwrap();
        complete(&mut app).await;
        let details = app.state.details.as_ref().unwrap();
        assert!(!details.title.contains(&id));
        assert!(!details.lines.join("\n").contains(&id));
    }
}

#[tokio::test]
async fn restored_tool_ids_use_current_redaction_and_partial_replies_remain_identifiable() {
    let (_root, mut app) = fixture();
    let mut prepared = session::create(&app.config, saya_app::new_session(&app.config).unwrap())
        .await
        .unwrap();
    app.secrets.push("current-private-fixture".into());
    prepared.session.history = vec![
        HistoryItem::ToolCall(ToolCall {
            call_id: "old-current-private-fixture".into(),
            name: "read_file".into(),
            arguments: "{}".into(),
        }),
        HistoryItem::AssistantPartial("unfinished reply".into()),
    ];
    app.finish_ui(Ok(UiResult::Session {
        prepared,
        resumed: true,
    }))
    .await
    .unwrap();
    assert!(!transcript(&app).contains("current-private-fixture"));
    assert!(app.state.messages.iter().any(|message| {
        message.text == "unfinished reply" && message.label.contains("interrupted partial")
    }));
    assert!(app.active.is_none());
}

#[tokio::test]
async fn session_initialization_lock_does_not_block_the_ui_executor() {
    let (_root, mut app) = fixture();
    std::fs::create_dir_all(&app.config.data_dir).unwrap();
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(app.config.data_dir.join(".database-init.lock"))
        .unwrap();
    // SAFETY: this temporary file owns a live descriptor for this test.
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    app.service("/new", "").await.unwrap();
    let started = Instant::now();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let elapsed = started.elapsed();
    app.cancel();
    drop(lock);
    let result = outcome(&mut app).await;
    app.finish_ui(result).await.unwrap();
    assert!(
        elapsed < Duration::from_millis(750),
        "SQLite initialization froze the UI executor for {elapsed:?}"
    );
    assert!(app.prepared.is_none());
    assert!(app.active.is_none());
}
