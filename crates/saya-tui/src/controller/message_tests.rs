//! Request ordering, final drains and user-only fold controls.
use super::control_tests::{approval, attach_active_call, fixture, stop_active_call};
use super::*;
use saya_app::thinking::ThinkingSnapshot;
use saya_core::{
    contract::{ErrorInfo, ModelProtocol, ToolCall, ToolResult},
    tools::todo::TodoStatus,
};

#[tokio::test]
async fn every_storage_failure_is_fatal_after_owned_run_cleanup() {
    for code in ["storage_busy", "storage_corrupt", "storage_sensitive_data"] {
        let (_root, mut app) = fixture();
        attach_active_call(&mut app).await;
        app.cancel();
        let (_, session) = (&mut app.active.as_mut().unwrap().join).await.unwrap();
        let error = BoundaryError(
            ErrorInfo {
                code: code.into(),
                message: "storage boundary failed".into(),
            },
            None,
            None,
        );
        assert!(app.finish(Ok((Err(error), session))).await.is_err());
        assert!(app.active.is_none());
        assert_eq!(app.state.status, "Failed");
    }
}
use serde_json::json;
use tokio::sync::oneshot;

fn event(app: &mut App, payload: EventPayload) {
    app.event(Event {
        schema_version: 1,
        session_id: app
            .prepared
            .as_ref()
            .map_or_else(saya_core::contract::SessionId::new, |prepared| {
                prepared.session.id
            }),
        run_id: None,
        seq: 1,
        timestamp: "fixture".into(),
        payload,
    });
}
fn started(app: &mut App, id: &str) {
    event(
        app,
        EventPayload::ModelStarted {
            request_id: id.into(),
            purpose: ModelPurpose::Execution,
            profile: "fixture".into(),
        },
    );
}
fn finished(app: &mut App, id: &str) {
    event(
        app,
        EventPayload::ModelFinished {
            request_id: id.into(),
            purpose: ModelPurpose::Execution,
            ok: true,
            usage: None,
            error: None,
        },
    );
}
fn thought(id: &str, text: impl Into<String>, active: bool) -> ThinkingSnapshot {
    ThinkingSnapshot {
        request_id: id.into(),
        text: text.into(),
        active,
        available: true,
        truncated: false,
    }
}
fn render(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| view::draw(frame, &mut app.state))
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}
fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}
fn todo_result(values: serde_json::Value) -> ToolResult {
    ToolResult {
        ok: true,
        data: Some(json!({"todos": values})),
        error: None,
        truncated: false,
    }
}
fn todo_history(id: &str, values: serde_json::Value) -> [HistoryItem; 2] {
    [
        HistoryItem::ToolCall(ToolCall {
            name: "todo_write".into(),
            call_id: id.into(),
            arguments: json!({"todos": values}).to_string(),
        }),
        HistoryItem::ToolResult {
            call_id: id.into(),
            result: todo_result(values),
        },
    ]
}

#[tokio::test]
async fn thinking_log_rounds_attach_in_event_order_without_empty_answers_or_review_cards() {
    let (_root, mut app) = fixture();
    attach_active_call(&mut app).await;
    let (_sender, receiver) = watch::channel(ThinkingLog {
        entries: vec![
            thought("one", "first thought", false),
            thought("two", "second thought", false),
        ],
    });
    app.active.as_mut().unwrap().thinking = receiver;
    started(&mut app, "one");
    assert_eq!(app.state.messages.len(), 1);
    assert_eq!(app.state.messages[0].role, Role::Thinking);
    assert_eq!(app.state.messages[0].text, "first thought");
    event(
        &mut app,
        EventPayload::ModelTextDelta {
            request_id: "one".into(),
            text: "first answer".into(),
        },
    );
    assert_eq!(app.state.status, "Responding");
    finished(&mut app, "one");
    event(
        &mut app,
        EventPayload::ToolStarted {
            name: "read_file".into(),
            call_id: "read".into(),
        },
    );
    let tool_id = app.state.messages.back().unwrap().id;
    event(
        &mut app,
        EventPayload::ToolFinished {
            name: "read_file".into(),
            call_id: "read".into(),
            result: ToolResult {
                ok: true,
                data: Some(json!({"text":"result"})),
                error: None,
                truncated: false,
            },
        },
    );
    assert_eq!(app.state.messages.back().unwrap().id, tool_id);
    started(&mut app, "two");
    event(
        &mut app,
        EventPayload::ModelTextDelta {
            request_id: "two".into(),
            text: "final answer".into(),
        },
    );
    event(
        &mut app,
        EventPayload::ModelStarted {
            request_id: "review".into(),
            purpose: ModelPurpose::Review,
            profile: "fixture".into(),
        },
    );
    event(
        &mut app,
        EventPayload::ModelTextDelta {
            request_id: "review".into(),
            text: "private review".into(),
        },
    );
    assert_eq!(
        app.state
            .messages
            .iter()
            .map(|message| message.role)
            .collect::<Vec<_>>(),
        [
            Role::Thinking,
            Role::Assistant,
            Role::Tool,
            Role::Thinking,
            Role::Assistant
        ]
    );
    assert!(
        !app.state
            .messages
            .iter()
            .any(|message| message.text.contains("private review"))
    );
    assert_eq!(app.state.messages[3].text, "second thought");
    stop_active_call(&mut app).await;
    assert!(app.state.messages.iter().all(|message| !message.streaming));
    assert!(
        !app.state
            .messages
            .iter()
            .any(|message| message.role == Role::Assistant && message.text.is_empty())
    );
}

#[tokio::test]
async fn thinking_watch_updates_redact_complete_text_before_display_truncation() {
    let (_root, mut app) = fixture();
    attach_active_call(&mut app).await;
    let secret = "fixture-controller-private-token";
    app.secrets.push(secret.into());
    let (sender, receiver) = watch::channel(ThinkingLog::default());
    app.active.as_mut().unwrap().thinking = receiver;
    started(&mut app, "redacted");
    sender.send_replace(ThinkingLog {
        entries: vec![
            thought("future", "future must wait", false),
            thought(
                "redacted",
                format!("{}{secret}", "x".repeat(view::MAX_MESSAGE_BYTES - 8)),
                false,
            ),
        ],
    });
    assert!(app.refresh_thinking());
    assert!(!app.refresh_thinking());
    assert_eq!(app.state.messages.len(), 1);
    let message = &app.state.messages[0];
    assert!(message.truncated);
    assert!(!message.text.contains("fixture-controller"));
    assert!(!message.text.contains("future must wait"));
    assert!(message.text.len() <= view::MAX_MESSAGE_BYTES);
    finished(&mut app, "redacted");
    stop_active_call(&mut app).await;
}

#[tokio::test]
async fn evicted_thinking_entries_and_cancelled_runs_leave_no_waiting_cards() {
    let (_root, mut app) = fixture();
    attach_active_call(&mut app).await;
    started(&mut app, "evicted");
    let (_sender, receiver) = watch::channel(ThinkingLog {
        entries: vec![thought("later", "later text", false)],
    });
    app.active.as_mut().unwrap().thinking = receiver;
    finished(&mut app, "evicted");
    assert_eq!(app.state.messages[0].label, "unavailable");
    assert!(app.state.messages[0].text.contains("retained display log"));
    started(&mut app, "cancelled");
    stop_active_call(&mut app).await;
    for message in app
        .state
        .messages
        .iter()
        .filter(|message| message.role == Role::Thinking)
    {
        assert!(!message.streaming);
        assert!(!message.label.starts_with("waiting"));
        assert!(!message.text.starts_with("Waiting"));
    }
}

#[tokio::test]
async fn final_event_drain_installs_tool_results_todos_and_thinking_without_duplicate_cards() {
    let (_root, mut app) = fixture();
    attach_active_call(&mut app).await;
    let (_thinking_sender, thinking) = watch::channel(ThinkingLog {
        entries: vec![thought("queued", "queued thought", false)],
    });
    app.active.as_mut().unwrap().thinking = thinking;
    let (sender, receiver) = mpsc::channel(8);
    app.active.as_mut().unwrap().events = receiver;
    let session_id = app.prepared.as_ref().unwrap().session.id;
    for payload in [
        EventPayload::ModelStarted {
            request_id: "queued".into(),
            purpose: ModelPurpose::Execution,
            profile: "fixture".into(),
        },
        EventPayload::ToolStarted {
            name: "todo_write".into(),
            call_id: "todo-queued".into(),
        },
        EventPayload::ToolFinished {
            name: "todo_write".into(),
            call_id: "todo-queued".into(),
            result: todo_result(
                json!([{"id":"one","content":"queued task","status":"in_progress"}]),
            ),
        },
        EventPayload::ModelTextDelta {
            request_id: "queued".into(),
            text: "queued answer".into(),
        },
        EventPayload::ModelFinished {
            request_id: "queued".into(),
            purpose: ModelPurpose::Execution,
            ok: true,
            usage: None,
            error: None,
        },
    ] {
        sender
            .send(Event {
                schema_version: 1,
                session_id,
                run_id: None,
                seq: 1,
                timestamp: "fixture".into(),
                payload,
            })
            .await
            .unwrap();
    }
    app.cancel();
    let (outcome, mut session) = (&mut app.active.as_mut().unwrap().join).await.unwrap();
    session.history.extend(todo_history(
        "todo-queued",
        json!([{"id":"one","content":"queued task","status":"in_progress"}]),
    ));
    app.finish(Ok((outcome, session))).await.unwrap();
    assert_eq!(app.state.todos.len(), 1);
    assert_eq!(app.state.todos[0].status, TodoStatus::InProgress);
    let tools = app
        .state
        .messages
        .iter()
        .filter(|message| message.role == Role::Tool)
        .collect::<Vec<_>>();
    assert_eq!(tools.len(), 1);
    assert!(tools[0].label.contains("todo-queued · completed"));
    assert!(tools[0].text.contains("queued task"));
    assert!(
        app.state
            .messages
            .iter()
            .any(|message| message.role == Role::Thinking && message.text == "queued thought")
    );
    assert!(
        app.state
            .messages
            .iter()
            .any(|message| message.role == Role::Assistant && message.text == "queued answer")
    );
}

#[tokio::test]
async fn todo_snapshots_update_successfully_and_only_a_new_success_can_clear_them() {
    let (_root, mut app) = fixture();
    attach_active_call(&mut app).await;
    let first = todo_result(json!([{"id":"one","content":"retained task","status":"pending"}]));
    event(
        &mut app,
        EventPayload::ToolFinished {
            name: "todo_write".into(),
            call_id: "one".into(),
            result: first,
        },
    );
    assert_eq!(app.state.todos.len(), 1);
    for (call_id, result) in [
        ("one", todo_result(json!([]))),
        (
            "failed",
            ToolResult {
                ok: false,
                data: Some(json!({"todos":[]})),
                error: None,
                truncated: false,
            },
        ),
        (
            "truncated",
            ToolResult {
                truncated: true,
                ..todo_result(json!([]))
            },
        ),
    ] {
        event(
            &mut app,
            EventPayload::ToolFinished {
                name: "todo_write".into(),
                call_id: call_id.into(),
                result,
            },
        );
        assert_eq!(app.state.todos.len(), 1);
    }
    event(
        &mut app,
        EventPayload::ToolFinished {
            name: "todo_write".into(),
            call_id: "clear".into(),
            result: todo_result(json!([])),
        },
    );
    assert!(app.state.todos.is_empty());
    stop_active_call(&mut app).await;
}

#[tokio::test]
async fn finish_reconciles_tasks_from_final_history_even_without_tool_events() {
    let (_root, mut app) = fixture();
    attach_active_call(&mut app).await;
    app.state.todos = vec![saya_core::tools::todo::TodoItem {
        id: "old".into(),
        content: "stale UI task".into(),
        status: TodoStatus::Pending,
    }];
    app.cancel();
    let (outcome, mut session) = (&mut app.active.as_mut().unwrap().join).await.unwrap();
    session.history.extend(todo_history(
        "saved-without-event",
        json!([{"id":"new","content":"canonical task","status":"completed"}]),
    ));
    app.finish(Ok((outcome, session))).await.unwrap();
    assert_eq!(app.state.todos.len(), 1);
    assert_eq!(app.state.todos[0].content, "canonical task");
    assert_eq!(app.state.todos[0].status, TodoStatus::Completed);

    attach_active_call(&mut app).await;
    app.cancel();
    let (outcome, mut session) = (&mut app.active.as_mut().unwrap().join).await.unwrap();
    session
        .history
        .extend(todo_history("saved-clear-without-event", json!([])));
    app.finish(Ok((outcome, session))).await.unwrap();
    assert!(app.state.todos.is_empty());
}

#[tokio::test]
async fn an_empty_final_text_does_not_create_an_empty_assistant_card() {
    let (_root, mut app) = fixture();
    attach_active_call(&mut app).await;
    started(&mut app, "no-body");
    app.cancel();
    let (outcome, session) = (&mut app.active.as_mut().unwrap().join).await.unwrap();
    let mut result = outcome.unwrap();
    result.text = Some(String::new());
    app.finish(Ok((Ok(result), session))).await.unwrap();
    assert!(
        !app.state
            .messages
            .iter()
            .any(|message| message.role == Role::Assistant)
    );
    assert!(
        app.state
            .messages
            .iter()
            .filter(|message| message.role == Role::Thinking)
            .all(|message| !message.streaming)
    );
}

#[tokio::test]
async fn joined_task_failure_takes_the_handle_and_closes_thinking_and_unknown_tools() {
    let (_root, mut app) = fixture();
    attach_active_call(&mut app).await;
    started(&mut app, "interrupted");
    event(
        &mut app,
        EventPayload::ToolStarted {
            name: "read_file".into(),
            call_id: "missing-result".into(),
        },
    );
    let active = app.active.as_mut().unwrap();
    active.join.abort();
    let outcome = (&mut active.join).await;
    assert!(app.finish(outcome).await.is_err());
    assert!(app.active.is_none());
    assert_eq!(app.state.status, "Failed");
    assert!(app.state.messages.iter().all(|message| !message.streaming));
    assert!(
        app.state
            .messages
            .iter()
            .any(|message| message.role == Role::Tool && message.label.contains("unknown"))
    );
    assert!(
        !app.state
            .messages
            .iter()
            .any(|message| message.text.starts_with("Waiting"))
    );
}

#[tokio::test]
async fn copying_or_clearing_a_selection_preserves_the_pending_approval_and_draft() {
    let (_root, mut app) = fixture();
    app.state.clear_messages();
    app.state.push_message(Role::User, "You", "COPY_ME");
    let (request, mut receiver) = approval(&app, "copy-control");
    app.request(request);
    app.state.editor.set_text("draft stays here");
    render(&mut app, 160, 45);
    app.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, 3));
    assert!(app.mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 8, 3)));
    app.mouse(mouse(MouseEventKind::Up(MouseButton::Left), 8, 3));
    app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert_eq!(
        app.state.take_clipboard_request().as_deref(),
        Some("COPY_ME")
    );
    assert_eq!(app.state.editor.text(), "draft stays here");
    assert!(app.pending.is_some());
    assert!(matches!(
        receiver.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    render(&mut app, 160, 45);
    app.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, 3));
    app.mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 8, 3));
    app.mouse(mouse(MouseEventKind::Up(MouseButton::Left), 8, 3));
    app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(!app.state.copy_text_selection());
    assert!(app.pending.is_some());
    assert!(matches!(
        receiver.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    app.reply_approval(false);
    assert_eq!(receiver.await.unwrap().unwrap(), Some(false));
}

#[tokio::test]
async fn mouse_and_ctrl_o_toggle_actual_cards_without_deciding_approval_or_editing_input() {
    let (_root, mut app) = fixture();
    app.state
        .push_message(Role::Thinking, "provider", "displayed thought marker");
    assert!(!render(&mut app, 80, 24).contains("displayed thought marker"));
    assert!(!app.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 3, 2)));
    assert!(app.mouse(mouse(MouseEventKind::Up(MouseButton::Left), 3, 2)));
    assert!(render(&mut app, 80, 24).contains("displayed thought marker"));
    assert!(!app.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 3, 3)));
    assert!(!app.mouse(mouse(MouseEventKind::Up(MouseButton::Left), 3, 3)));
    app.key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert!(!render(&mut app, 80, 24).contains("displayed thought marker"));
    app.state.editor.set_text("/per");
    app.key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert!(!render(&mut app, 80, 24).contains("displayed thought marker"));
    assert_eq!(app.state.editor.text(), "/per");
    app.state.editor.clear();
    let (request, mut receiver) = approval(&app, "fold-control");
    app.request(request);
    app.key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert!(matches!(
        receiver.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    assert!(app.pending.is_some());
    assert_eq!(
        app.state.details.as_ref().unwrap().title,
        "Approval required"
    );
    assert!(app.state.editor.text().is_empty());
    render(&mut app, 160, 45);
    assert!(app.mouse(mouse(MouseEventKind::ScrollUp, 3, 3)));
    assert_eq!(app.state.scroll, 3);
    assert!(app.mouse(mouse(MouseEventKind::ScrollDown, 82, 3)));
    assert_eq!(app.state.details_scroll, 3);
    assert!(!app.mouse(mouse(MouseEventKind::ScrollUp, 3, 41)));
    app.reply_approval(false);
    assert_eq!(receiver.await.unwrap().unwrap(), Some(false));
}

#[tokio::test]
async fn restore_uses_current_profile_and_redacts_native_reasoning_before_its_display_limit() {
    let (_root, mut app) = fixture();
    let mut prepared = session::create(&app.config, saya_app::new_session(&app.config).unwrap())
        .await
        .unwrap();
    let secret = "fixture-restored-private-token";
    app.secrets.push(secret.into());
    let values = json!([{"id":"one","content":"restored task","status":"completed"}]);
    prepared.session.history = vec![
        HistoryItem::UserText("restore task".into()),
        HistoryItem::ProviderItem {
            protocol: ModelProtocol::ChatCompletions,
            value: json!({"reasoning_content":format!("{}{secret}", "x".repeat(view::MAX_MESSAGE_BYTES - 8)), "signature":"opaque-never-display"}),
        },
        HistoryItem::AssistantText("stored answer".into()),
        HistoryItem::ToolCall(ToolCall {
            name: "todo_write".into(),
            call_id: "stored-todo".into(),
            arguments: json!({"todos":values}).to_string(),
        }),
        HistoryItem::ToolResult {
            call_id: "stored-todo".into(),
            result: todo_result(values),
        },
    ];
    app.config.profiles.get_mut("fixture").unwrap().model = "restored-model".into();
    app.state.model = "stale-model".into();
    app.finish_ui(Ok(services::UiResult::Session {
        prepared,
        resumed: true,
    }))
    .await
    .unwrap();
    let reasoning = app
        .state
        .messages
        .iter()
        .find(|message| message.role == Role::Thinking)
        .unwrap();
    assert!(reasoning.truncated);
    assert!(!reasoning.text.contains("fixture-restored"));
    assert!(!reasoning.text.contains("opaque-never-display"));
    assert!(!reasoning.streaming);
    assert_eq!(app.state.todos.len(), 1);
    assert_eq!(app.state.todos[0].status, TodoStatus::Completed);
    assert!(
        app.state
            .messages
            .iter()
            .any(|message| message.role == Role::Assistant && message.label == "restored-model")
    );
    assert_eq!(
        app.state
            .messages
            .iter()
            .filter(|message| message.role == Role::Tool)
            .count(),
        1
    );
    assert_eq!(app.state.status, "Ready");
}

#[test]
fn mcp_connection_state_requires_matching_host_events_and_resets_for_the_next_run() {
    use saya_core::contract::{McpServerState, McpServerStatus, McpTransport};
    let (_root, mut app) = fixture();
    app.config.mcp = serde_json::from_value(json!({"servers":{
        "fixture":{"transport":{"type":"stdio","argv":["/usr/bin/true"]}},
        "disabled":{"enabled":false,"transport":{"type":"stdio","argv":["/usr/bin/true"]}}
    }}))
    .unwrap();
    app.reset_mcp_statuses();
    assert_eq!(app.state.mcp_servers.len(), 1);
    assert_eq!(app.state.mcp_servers[0].state, McpServerState::Configured);
    let status = |server: &str, transport, state, tools| McpServerStatus {
        server: server.into(),
        transport,
        state,
        tools,
    };
    event(
        &mut app,
        EventPayload::McpStatus {
            servers: vec![
                status("unknown", McpTransport::Stdio, McpServerState::Connected, 9),
                status(
                    "disabled",
                    McpTransport::Stdio,
                    McpServerState::Connected,
                    9,
                ),
                status("fixture", McpTransport::Http, McpServerState::Connected, 9),
            ],
        },
    );
    assert_eq!(app.state.mcp_servers[0].state, McpServerState::Configured);
    for (state, tools) in [
        (McpServerState::Connecting, 0),
        (McpServerState::Connected, 3),
        (McpServerState::Stopped, 0),
        (McpServerState::NeedsAuth, 0),
        (McpServerState::Failed, 0),
    ] {
        event(
            &mut app,
            EventPayload::McpStatus {
                servers: vec![status("fixture", McpTransport::Stdio, state, tools)],
            },
        );
        assert_eq!(app.state.mcp_servers[0].state, state);
        assert_eq!(app.state.mcp_servers[0].tools, tools);
    }
    app.reset_mcp_statuses();
    assert_eq!(app.state.mcp_servers[0].state, McpServerState::Configured);
    assert_eq!(app.state.mcp_servers[0].tools, 0);
}
