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
    assert_eq!(app.state.session_title, "saved question");
    assert_eq!(app.state.context_input_tokens, None);
    assert!(app.state.context_breakdown.is_empty());
    assert_eq!(app.state.skills_count, None);
    assert!(snapshot(&app).await.runs.is_empty());
    assert_eq!(store.lock_session(id).err().unwrap().0.code, "session_busy");
    let data = app.config.data_dir.clone();
    drop(app);
    let _released = SqliteStore::lock_existing(&data, id).unwrap();
}

#[tokio::test]
async fn sessions_selector_renders_saved_metadata_and_resumes_the_selected_id() {
    let (_root, mut app) = fixture();
    create(&mut app).await;
    let mut first = app.prepared.take().unwrap();
    let first_id = first.session.id;
    session::append(&mut first, "saved selector task".into())
        .await
        .unwrap();
    drop(first);
    create(&mut app).await;
    app.service("/sessions", "").await.unwrap();
    complete(&mut app).await;
    let panel = app.state.picker_panel.as_mut().unwrap();
    assert_eq!(panel.kind, crate::view::PickerKind::Sessions);
    assert!(panel.items.iter().any(|item| {
        item.id == first_id.0.to_string()
            && item.label == "saved selector task"
            && item.details.iter().any(|line| line.contains("Workspace"))
    }));
    panel.selected = panel
        .items
        .iter()
        .position(|item| item.id == first_id.0.to_string())
        .unwrap();
    app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    complete(&mut app).await;
    assert_eq!(app.prepared.as_ref().unwrap().session.id, first_id);
    assert_eq!(app.state.session_title, "saved selector task");
}

#[tokio::test]
async fn resumed_session_restores_context_usage_from_run_metrics() {
    let (_root, mut app) = fixture();
    let prepared = session::create(&app.config, saya_app::new_session(&app.config).unwrap())
        .await
        .unwrap();
    let id = prepared.session.id;
    let run = Run {
        id: RunId::new(),
        session_id: id,
        work_mode: WorkMode::Work,
        permissions: PermissionMode::Auto,
    };
    prepared
        .store
        .commit(&[
            StoreChange::RunStarted(run.clone()),
            StoreChange::RunMetrics {
                run_id: run.id,
                metrics: saya_core::metrics::RunMetrics {
                    execution_requests: 1,
                    review_requests: 0,
                    compaction_requests: 0,
                    tool_calls: 0,
                    tool_executions: 0,
                    duration_ms: 1,
                    execution_usage: Some(Usage {
                        input_tokens: Some(42_000),
                        output_tokens: Some(1_000),
                        estimated: false,
                    }),
                    review_usage: None,
                    compaction_usage: None,
                    execution_cost: None,
                    review_cost: None,
                    compaction_cost: None,
                },
            },
        ])
        .await
        .unwrap();
    drop(prepared);
    app.service("/resume", &id.0.to_string()).await.unwrap();
    complete(&mut app).await;
    assert_eq!(app.state.context_input_tokens, Some(42_000));
    assert_eq!(app.state.context_breakdown.len(), 1);
    assert_eq!(app.state.context_breakdown[0].label, "Last run");
    assert_eq!(app.state.context_breakdown[0].tokens, 42_000);
}

#[tokio::test]
async fn empty_tab_cycles_from_the_latest_mode_deferred_behind_ui_cleanup() {
    let (_root, mut app) = fixture();
    let (sender, cancel) = controlled_job(&mut app);
    for expected in [WorkMode::Code, WorkMode::Plan] {
        app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(app.deferred_mode, Some(expected));
        assert_eq!(app.config.mode, WorkMode::Work);
        assert!(cancel.is_cancelled());
    }
    assert!(
        sender
            .send(UiResult::Details(crate::view::Details {
                title: "cancelled query".into(),
                lines: vec![],
            }))
            .is_ok()
    );
    complete(&mut app).await;
    assert_eq!(app.config.mode, WorkMode::Plan);
    assert_eq!(app.state.mode, "PLAN");
    assert!(app.deferred_mode.is_none());
    assert!(app.state.editor.text().is_empty());
}

#[tokio::test]
async fn session_title_uses_the_first_task_and_redacts_before_shortening() {
    let (_root, mut app) = fixture();
    assert_eq!(app.state.session_title, "New session");
    assert!(app.state.messages.is_empty());
    assert_eq!(app.state.context_window_tokens, Some(128000));
    assert_eq!(app.state.thinking_effort, None);
    create(&mut app).await;
    let secret = "fixture-sensitive-title";
    app.secrets.push(secret.into());
    let first = format!("  调查\n\t{}{}  完成  ", "项".repeat(72), secret);
    session::append(app.prepared.as_mut().unwrap(), first)
        .await
        .unwrap();
    app.refresh_header();
    let title = app.state.session_title.clone();
    assert_eq!(title.chars().count(), 80);
    assert!(title.starts_with("调查 "));
    assert!(!title.contains("fixture"));
    assert!(!title.contains('\n'));
    session::append(app.prepared.as_mut().unwrap(), "later task".into())
        .await
        .unwrap();
    app.refresh_header();
    assert_eq!(app.state.session_title, title);
    app.record_context_input(Some(Usage {
        input_tokens: Some(512),
        output_tokens: Some(20),
        estimated: true,
    }));
    app.record_context_snapshot(ContextSnapshot {
        entries: vec![saya_app::context::ContextEntry {
            label: "Skills",
            tokens: 20,
        }],
        skills_count: Some(1),
    });
    create(&mut app).await;
    assert_eq!(app.state.session_title, "New session");
    assert_eq!(app.state.context_input_tokens, None);
    assert!(!app.state.context_estimated);
    assert!(app.state.context_breakdown.is_empty());
    assert_eq!(app.state.skills_count, None);
    assert!(snapshot(&app).await.runs.is_empty());
}

#[tokio::test]
async fn profile_selection_clears_the_title_and_previous_context_usage() {
    let (_root, mut app) = fixture();
    create(&mut app).await;
    session::append(app.prepared.as_mut().unwrap(), "old task".into())
        .await
        .unwrap();
    app.record_context_input(Some(Usage {
        input_tokens: Some(512),
        output_tokens: None,
        estimated: false,
    }));
    app.record_context_snapshot(ContextSnapshot {
        entries: vec![saya_app::context::ContextEntry {
            label: "System",
            tokens: 30,
        }],
        skills_count: Some(0),
    });
    let mut other = app.config.profiles["fixture"].clone();
    other.model = "other model".into();
    other.context_window_tokens = 256000;
    other.thinking_effort = Some(saya_core::config::ThinkingEffort::Max);
    app.config.profiles.insert("other".into(), other);
    app.service("/profile", "other").await.unwrap();
    assert!(app.prepared.is_none());
    assert_eq!(app.state.session_title, "New session");
    assert_eq!(app.state.context_window_tokens, Some(256000));
    assert_eq!(app.state.context_input_tokens, None);
    assert!(app.state.context_breakdown.is_empty());
    assert_eq!(app.state.skills_count, None);
    assert_eq!(app.state.model, "other model");
    assert_eq!(app.state.thinking_effort.as_deref(), Some("max"));
}

#[tokio::test]
async fn workspace_and_effort_changes_are_persisted_with_the_open_session() {
    let (root, mut app) = fixture();
    create(&mut app).await;
    let migrated = root.path().join("migrated");
    std::fs::create_dir(&migrated).unwrap();
    app.command(&format!("/cd {}", migrated.display()))
        .await
        .unwrap();
    app.command("/effort max").await.unwrap();
    let prepared = app.prepared.as_ref().unwrap();
    let stored = prepared
        .store
        .load_session(prepared.session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.workspace, migrated.canonicalize().unwrap());
    assert_eq!(
        prepared
            .store
            .fingerprint(prepared.session.id)
            .await
            .unwrap(),
        app.config.profiles["fixture"].fingerprint().unwrap()
    );
    assert_eq!(app.state.thinking_effort.as_deref(), Some("max"));
    let data_dir = app.config.data_dir.clone();
    let session_id = prepared.session.id;
    drop(app);
    let _lock = SqliteStore::lock_existing(&data_dir, session_id).unwrap();
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
        if command == "/sessions" {
            assert_eq!(
                app.state.picker_panel.as_ref().unwrap().kind,
                crate::view::PickerKind::Sessions
            );
            app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
                .await
                .unwrap();
        } else if command == "/tools" {
            let text = app.state.details.as_ref().unwrap().lines.join("\n");
            assert!(text.contains("unknown"));
            assert!(text.contains("Output truncated by configured limits."));
            assert!(text.contains("Error: truncated_fixture"));
            assert!(text.contains("Message: bounded output"));
            assert!(text.contains("text: retained prefix"));
            assert!(text.contains("reused-call"));
            for run in &ids {
                assert!(text.contains(&run.0.to_string()));
            }
        } else {
            assert!(app.state.details.as_ref().unwrap().title.contains("UI"));
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
                resumed: false,
                usage: None,
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
        if let Some(panel) = &app.state.picker_panel {
            assert!(!panel.title.contains(&id));
            assert!(panel.items.iter().all(|item| {
                !item.label.contains(&id)
                    && !item.description.contains(&id)
                    && !item.details.join("\n").contains(&id)
            }));
            app.state.picker_panel = None;
        } else {
            let details = app.state.details.as_ref().unwrap();
            assert!(!details.title.contains(&id));
            assert!(!details.lines.join("\n").contains(&id));
        }
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
        usage: None,
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

#[tokio::test]
async fn failed_initialization_preserves_a_newer_draft_and_the_unsent_prompt() {
    for draft in ["", "newer draft 🌸"] {
        let (root, mut app) = fixture();
        let secret = "fixture-private-token";
        app.secrets.push(secret.into());
        app.config.data_dir = root.path().join("not-a-directory");
        std::fs::write(&app.config.data_dir, "block database initialization").unwrap();
        app.start(format!("earlier task {secret}")).await.unwrap();
        assert!(app.queued_prompt.is_some());
        app.state.editor.set_text(draft);
        let result = outcome(&mut app).await;
        assert!(result.is_err());
        app.finish_ui(result).await.unwrap();
        if draft.is_empty() {
            assert_eq!(app.state.editor.text(), format!("earlier task {secret}"));
        } else {
            assert_eq!(app.state.editor.text(), draft);
            let unsent = app
                .state
                .messages
                .iter()
                .find(|message| message.label == "Unsent draft")
                .unwrap();
            assert!(unsent.text.contains("earlier task"));
            assert!(!unsent.text.contains(secret));
            assert!(transcript(&app).contains("Your current draft is preserved"));
        }
        assert!(app.active.is_none() && app.prepared.is_none());
        assert!(app.queued_prompt.is_none());
    }
}

#[tokio::test]
async fn failed_prompt_handoff_preserves_a_newer_draft_and_the_unsent_prompt() {
    let (_root, mut app) = fixture();
    create(&mut app).await;
    app.queued_prompt = Some("earlier task".into());
    app.state.editor.set_text("newer draft");
    app.config.profiles.clear();
    assert!(
        app.finish_ui(Ok(UiResult::Details(view::Details {
            title: "fixture".into(),
            lines: vec![],
        })))
        .await
        .is_err()
    );
    assert_eq!(app.state.editor.text(), "newer draft");
    assert!(
        app.state
            .messages
            .iter()
            .any(|message| { message.label == "Unsent draft" && message.text == "earlier task" })
    );
    assert!(app.active.is_none());
    assert!(app.prepared.as_ref().unwrap().session.history.is_empty());
}

#[tokio::test]
async fn recoverable_handoff_failure_keeps_the_ui_and_clears_deferred_mode() {
    let (_root, mut app) = fixture();
    create(&mut app).await;
    app.queued_prompt = Some("earlier task".into());
    app.state.editor.set_text("newer draft");
    app.deferred_mode = Some(WorkMode::Plan);
    app.state.pending_mode = Some("PLAN".into());
    app.config.profiles.clear();
    let error = app
        .finish_ui(Ok(UiResult::Details(view::Details {
            title: "fixture".into(),
            lines: vec![],
        })))
        .await
        .unwrap_err();
    app.report_error(error).unwrap();
    assert_eq!(app.state.editor.text(), "newer draft");
    assert!(transcript(&app).contains("earlier task"));
    assert!(app.deferred_mode.is_none());
    assert!(app.state.pending_mode.is_none());
    assert!(app.ui_job.is_none() && app.active.is_none());
    assert!(app.exit_after_run.is_none());

    for code in ["storage_busy", "storage_corrupt", "storage_sensitive_data"] {
        let error = BoundaryError(
            ErrorInfo {
                code: code.into(),
                message: "durable boundary failed".into(),
            },
            None,
            None,
        );
        assert!(app.report_error(Box::new(error)).is_err());
    }
}

#[tokio::test]
async fn cancelling_ui_work_preserves_current_and_queued_drafts() {
    for draft in ["", "newer draft 🌸"] {
        let (_root, mut app) = fixture();
        app.start("original first task".into()).await.unwrap();
        app.state.editor.set_text(draft);
        app.cancel();
        assert!(app.queued_prompt.is_none());
        assert_eq!(
            app.state.editor.text(),
            if draft.is_empty() {
                "original first task"
            } else {
                draft
            }
        );
        if !draft.is_empty() {
            assert!(transcript(&app).contains("original first task"));
        }
        let result = outcome(&mut app).await;
        app.finish_ui(result).await.unwrap();
        assert!(app.active.is_none());
    }
    let (_root, mut app) = fixture();
    let (sender, _) = controlled_job(&mut app);
    app.state.editor.set_text("draft during query");
    app.cancel();
    assert_eq!(app.state.editor.text(), "draft during query");
    sender
        .send(UiResult::Details(view::Details {
            title: "stale".into(),
            lines: vec![],
        }))
        .unwrap_or_else(|_| panic!("UI receiver closed"));
    complete(&mut app).await;
    assert_eq!(app.state.editor.text(), "draft during query");
}

#[tokio::test]
async fn explicit_tui_resume_uses_saved_metadata_after_startup_overrides_and_profile_selection() {
    let (root, mut app) = fixture();
    let mut other = app.config.profiles["fixture"].clone();
    other.model = "other model".into();
    app.config.profiles.insert("other".into(), other);
    let workspace = root.path().join("saved-workspace");
    std::fs::create_dir(&workspace).unwrap();
    let workspace = std::fs::canonicalize(workspace).unwrap();
    let mut saved = app.config.clone();
    saved.cwd = workspace.clone();
    saved.mode = WorkMode::Plan;
    let prepared = session::create(&saved, saya_app::new_session(&saved).unwrap())
        .await
        .unwrap();
    let id = prepared.session.id;
    drop(prepared);
    app.options.cwd_explicit = true;
    app.options.profile_explicit = true;
    app.options.mode_override = Some(WorkMode::Code);
    app.config.mode = WorkMode::Code;
    app.state.status = "NeedsInput".into();
    app.service("/profile", "other").await.unwrap();
    assert_eq!(app.state.status, "Ready");
    app.command("/mode plan").await.unwrap();
    app.service("/resume", &id.0.to_string()).await.unwrap();
    complete(&mut app).await;
    let prepared = app.prepared.as_ref().unwrap();
    assert_eq!(prepared.session.id, id);
    assert_eq!(prepared.session.workspace, workspace);
    assert_eq!(prepared.session.work_mode, WorkMode::Plan);
    assert_eq!(prepared.session.profile, "fixture");
    let stored = prepared.store.load_session(id).await.unwrap().unwrap();
    assert_eq!(stored.workspace, workspace);
    assert_eq!(stored.work_mode, WorkMode::Plan);
    assert_eq!(stored.profile, "fixture");
    assert!(app.active.is_none());
}

#[tokio::test]
async fn titles_redact_after_whitespace_and_terminal_character_normalization() {
    let (_root, mut app) = fixture();
    create(&mut app).await;
    for (secret, text) in [
        ("fixture-secret", "fixture-\u{200e}secret"),
        ("fixture-secret", "fixture-\u{206f}secret"),
        ("fixture-secret", "fixture-\u{1b}secret"),
        ("fixture secret", "fixture \n\t secret"),
    ] {
        app.secrets = vec![secret.into()];
        app.prepared.as_mut().unwrap().session.history = vec![HistoryItem::UserText(text.into())];
        app.refresh_header();
        assert!(!app.state.session_title.contains(secret));
        let title = app.safe_title(&format!("Saya | {}", app.state.session_title));
        assert!(!title.contains(secret));
        assert_eq!(title, crate::terminal::safe_title(&title));
        assert!(title.contains("[REDACTED]"));
    }
}
