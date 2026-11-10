use super::*;
use saya_core::contract::RunStatus;
use serde_json::{Value, json};
use std::{sync::mpsc as blocking, time::Duration};

#[path = "../../../saya-core/tests/fixtures/http_support/mod.rs"]
mod http;

fn answer(text: String) -> String {
    json!({"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":text}}]}).to_string()
}
fn summary(request: &Value) -> String {
    let data: Value = serde_json::from_str(
        request["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let mut objectives = Vec::new();
    for unit in data["source_units"].as_array().unwrap() {
        for source in unit.as_array().unwrap() {
            if source["role"] == "user" {
                objectives.push(json!({"text":"Continue the user task with its recorded constraints.","source_ids":[source["source_id"]],"evidence":"user_request","status":"active"}));
            }
        }
    }
    answer(json!({"schema_version":1,"covered_range":data["assigned_source_range"],"objective":objectives,
        "constraints":[],"decisions":[],"completed":[],"remaining":[],"uncertainties":[],"required_ids":[],"references":[]}).to_string())
}
async fn prepared(app: &mut App) {
    let mut session = saya_app::new_session(&app.config).unwrap();
    session.history = vec![
        HistoryItem::UserText("Keep the selected evidence and complete the task".into()),
        HistoryItem::AssistantText("Earlier repetitive observation. ".repeat(1000)),
        HistoryItem::UserText("Continue with the same requirements".into()),
    ];
    app.prepared = Some(session::create(&app.config, session).await.unwrap());
    app.refresh_header();
}
async fn finish(app: &mut App) {
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        &mut app.active.as_mut().unwrap().join,
    )
    .await
    .unwrap();
    app.finish(result).await.unwrap();
}
async fn signal(app: &mut App, receiver: &blocking::Receiver<()>) {
    let entered = async {
        loop {
            match receiver.try_recv() {
                Ok(()) => break,
                Err(blocking::TryRecvError::Empty) => {}
                Err(blocking::TryRecvError::Disconnected) => {
                    panic!("Controlled model fixture closed before receiving the request");
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    tokio::select! {
        result = &mut app.active.as_mut().unwrap().join => {
            let outcome = result.map(|(outcome, _)| outcome);
            panic!("Run ended before the controlled model request became active: {outcome:?}");
        }
        result = tokio::time::timeout(Duration::from_secs(10), entered) => {
            result.expect("Controlled model request did not become active within ten seconds");
        }
    }
}

#[tokio::test]
async fn compact_command_requires_an_existing_session_and_preserves_the_editor_draft() {
    let (_root, mut app) = super::control_tests::fixture();
    app.state.editor.set_text("draft to keep");
    assert!(app.command("/compact").await.is_err());
    assert!(app.command("/compact extra").await.is_err());
    assert_eq!(app.state.editor.text(), "draft to keep");
    assert!(app.prepared.is_none() && app.active.is_none() && app.ui_job.is_none());
    assert!(!app.config.data_dir.exists());
    app.state.editor.set_text("/co");
    app.refresh_completion();
    assert_eq!(app.state.completion.current().unwrap().text, "/compact");
}

#[tokio::test]
async fn idle_compact_uses_the_real_controller_without_a_user_message_or_execution_call() {
    let (_root, mut app) = super::control_tests::fixture();
    let (url, requests) =
        http::server_dynamic(1, |_, request| ("application/json", summary(request))).unwrap();
    app.config.profiles.get_mut("fixture").unwrap().base_url = url;
    prepared(&mut app).await;
    app.record_context_input(Some(saya_core::contract::Usage {
        input_tokens: Some(42_000),
        output_tokens: Some(1_000),
        estimated: false,
    }));
    app.state.context_breakdown = vec![crate::view::ContextEntry {
        label: "Last request".into(),
        tokens: 42_000,
    }];
    let raw = app.prepared.as_ref().unwrap().session.history.clone();
    app.state.editor.set_text("/compact");
    app.submit().await.unwrap();
    assert!(app.state.editor.text().is_empty());
    assert!(
        app.active
            .as_ref()
            .is_some_and(|active| active.kind == RunKind::Compaction)
    );
    assert_eq!(app.state.status, "Compacting");
    assert_eq!(app.state.context_input_tokens, Some(42_000));
    assert_eq!(app.state.context_breakdown[0].label, "Last request");
    finish(&mut app).await;
    assert!(app.active.is_none());
    assert_eq!(app.state.context_input_tokens, Some(42_000));
    let current = app.prepared.as_ref().unwrap();
    assert_eq!(current.session.history, raw);
    assert_eq!(
        current
            .store
            .load_session(current.session.id)
            .await
            .unwrap()
            .unwrap()
            .history,
        raw
    );
    assert!(
        current
            .store
            .context_checkpoint(current.session.id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        !app.state
            .messages
            .iter()
            .any(|message| matches!(message.role, Role::User | Role::Assistant))
    );
    let request = requests.try_recv().unwrap();
    assert!(request.get("tools").is_none());
    assert!(requests.try_recv().is_err());
}

#[tokio::test]
async fn idle_compact_leaves_configured_mcp_processes_unstarted() {
    let (_root, mut app) = super::control_tests::fixture();
    let (url, _) =
        http::server_dynamic(1, |_, request| ("application/json", summary(request))).unwrap();
    app.config.profiles.get_mut("fixture").unwrap().base_url = url;
    let marker = app.config.cwd.join("mcp-started");
    app.config.mcp.servers.insert("sentinel".into(), serde_json::from_value(json!({
        "enabled":true,"transport":{"type":"stdio","argv":["/bin/sh","-c","printf started > \"$1\"","fixture",marker.to_str().unwrap()]}
    })).unwrap());
    prepared(&mut app).await;
    app.state.editor.set_text("unsent draft");
    app.state.command_palette = Some(
        COMMANDS
            .iter()
            .position(|command| command.text == "/compact")
            .unwrap(),
    );
    app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.state.editor.text(), "unsent draft");
    finish(&mut app).await;
    assert!(!marker.exists());
    assert!(
        app.prepared
            .as_ref()
            .unwrap()
            .store
            .context_checkpoint(app.prepared.as_ref().unwrap().session.id)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn queued_compaction_at_the_last_execution_call_runs_after_completion_without_a_second_user_turn()
 {
    let (root, mut app) = super::control_tests::fixture();
    assert!(app.config.memory.read && app.config.memory.inject && app.config.memory.write);
    let memory_root = app.config.memory_root().unwrap();
    assert_eq!(memory_root, root.path().join(".saya"));
    assert_eq!(
        app.config.cwd.parent().unwrap().canonicalize().unwrap(),
        root.path().canonicalize().unwrap()
    );
    assert!(!memory_root.exists());
    let (entered, ready) = blocking::channel();
    let (release, released) = blocking::channel();
    let (url, requests) = http::server_dynamic(2, move |index, request| {
        if index == 0 {
            entered.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(5)).unwrap();
            ("application/json", answer("Execution completed".into()))
        } else {
            ("application/json", summary(request))
        }
    })
    .unwrap();
    app.config.profiles.get_mut("fixture").unwrap().base_url = url;
    prepared(&mut app).await;
    let before = app.prepared.as_ref().unwrap().session.history.len();
    app.start("New current task".into()).await.unwrap();
    signal(&mut app, &ready).await;
    app.command("/compact").await.unwrap();
    assert!(app.active.as_ref().unwrap().handle.compaction_pending());
    release.send(()).unwrap();
    finish(&mut app).await;
    assert!(
        app.active
            .as_ref()
            .is_some_and(|active| active.kind == RunKind::Compaction)
    );
    let raw = app.prepared.as_ref().unwrap().session.history.clone();
    assert_eq!(raw.len(), before + 2);
    finish(&mut app).await;
    assert_eq!(app.prepared.as_ref().unwrap().session.history, raw);
    let requests = requests.try_iter().collect::<Vec<_>>();
    assert_eq!(requests.len(), 2);
    let tools = requests[0]["tools"].as_array().unwrap();
    for name in ["memory_read", "memory_update", "memory_forget"] {
        assert!(tools.iter().any(|tool| tool["function"]["name"] == name));
    }
    assert!(!memory_root.exists());
    assert_eq!(
        app.state
            .messages
            .iter()
            .filter(|message| message.role == Role::User)
            .count(),
        1
    );
}

#[tokio::test]
async fn failed_or_cancelled_compaction_keeps_history_lock_and_idle_controls_available() {
    for cancel in [false, true] {
        let (_root, mut app) = super::control_tests::fixture();
        let (entered, ready) = blocking::channel();
        let (release, released) = blocking::channel();
        let (url, _) = http::server_dynamic(1, move |_, _| {
            entered.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(5)).unwrap();
            ("application/json", answer("invalid summary output".into()))
        })
        .unwrap();
        app.config.profiles.get_mut("fixture").unwrap().base_url = url;
        prepared(&mut app).await;
        let raw = app.prepared.as_ref().unwrap().session.history.clone();
        app.command("/compact").await.unwrap();
        signal(&mut app, &ready).await;
        if cancel {
            app.cancel();
        }
        release.send(()).unwrap();
        finish(&mut app).await;
        let prepared = app.prepared.as_ref().unwrap();
        assert_eq!(prepared.session.history, raw);
        assert_eq!(
            prepared
                .store
                .lock_session(prepared.session.id)
                .err()
                .unwrap()
                .0
                .code,
            "session_busy"
        );
        assert!(
            prepared
                .store
                .context_checkpoint(prepared.session.id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(app.active.is_none());
        assert_eq!(
            app.state.status,
            if cancel {
                format!("{:?}", RunStatus::Cancelled)
            } else {
                format!("{:?}", RunStatus::Failed)
            }
        );
        app.command("/mode code").await.unwrap();
        assert_eq!(app.config.mode, WorkMode::Code);
    }
}

#[tokio::test]
async fn cancelling_a_ui_job_discards_its_queued_compaction() {
    let (_root, mut app) = super::control_tests::fixture();
    prepared(&mut app).await;
    let (reply, response) = tokio::sync::oneshot::channel();
    app.ui_job = Some(services::UiJob {
        cancel: Arc::new(RunCancellation::new()),
        join: tokio::spawn(async move { response.await.unwrap() }),
    });
    app.command("/compact").await.unwrap();
    assert!(app.queued_compaction);
    app.cancel();
    assert!(!app.queued_compaction);
    assert!(
        reply
            .send(Ok(services::UiResult::Details(view::Details {
                title: "UI fixture".into(),
                lines: Vec::new()
            })))
            .is_ok()
    );
    let result = services::next_job(&mut app.ui_job).await;
    app.finish_ui(result).await.unwrap();
    assert!(app.active.is_none());
    assert_eq!(
        app.prepared
            .as_ref()
            .unwrap()
            .store
            .load_session(app.prepared.as_ref().unwrap().session.id)
            .await
            .unwrap()
            .unwrap()
            .history,
        app.prepared.as_ref().unwrap().session.history
    );
}
