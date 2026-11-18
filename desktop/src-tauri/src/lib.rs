//! Tauri 2 in-process desktop shell for Soyo Harness.

pub mod commands;
pub mod redaction;
pub mod runtime_bridge;
pub mod tool_format;
pub mod types;

pub use runtime_bridge::DesktopState;
pub use types::{DesktopError, SessionItemDto, SessionPageDto, WorkModeDto};

use specta_typescript::Typescript;
use tauri::Manager;
use tauri_specta::{Builder, collect_commands};

pub fn specta_builder() -> Builder<tauri::Wry> {
    Builder::<tauri::Wry>::new().commands(collect_commands![
        commands::list_sessions,
        commands::get_bootstrap_state,
        commands::new_session,
        commands::submit_prompt,
        commands::cancel_run,
        commands::compact_session,
        commands::reply_approval,
        commands::reply_input,
        commands::set_work_mode,
        commands::set_permission_mode,
        commands::resume_session,
    ])
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = specta_builder();
    let desktop_state = DesktopState::from_environment();
    tauri::Builder::default()
        .manage(desktop_state)
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(builder.invoke_handler())
        .setup(move |app| {
            builder.mount_events(app);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Soyo desktop application")
        .run(|app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                let state = app_handle.state::<DesktopState>();
                tauri::async_runtime::block_on(state.shutdown());
            }
        });
}

pub fn export_bindings_to(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    specta_builder().export(Typescript::default(), path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_bridge::StreamSink;
    use crate::types::DesktopStreamEnvelope;
    use soyo_core::{
        boundary::{Cancellation, Model, ModelRequest, ModelStream, ModelUpdate, Operation},
        config::{self, Environment, Overrides},
        contract::{BoundaryError, HistoryItem, ModelPurpose, ToolCall, Usage},
    };
    use std::{
        collections::VecDeque,
        path::PathBuf,
        sync::{Arc, Mutex},
    };
    use tokio::sync::mpsc;

    struct ScriptedModel {
        chunks: Vec<String>,
        thinking: Option<String>,
        delay_ms: u64,
    }

    struct ScriptedStream {
        chunks: Vec<String>,
        thinking: Option<String>,
        step: usize,
        sent_thinking: bool,
        delay_ms: u64,
    }

    impl Model for ScriptedModel {
        fn start<'a>(
            &'a self,
            _request: &'a ModelRequest,
            _cancel: &'a dyn Cancellation,
        ) -> Operation<'a, Box<dyn ModelStream>> {
            let chunks = self.chunks.clone();
            let thinking = self.thinking.clone();
            let delay_ms = self.delay_ms;
            Box::pin(async move {
                Ok(Box::new(ScriptedStream {
                    chunks,
                    thinking,
                    step: 0,
                    sent_thinking: false,
                    delay_ms,
                }) as Box<dyn ModelStream>)
            })
        }
    }

    impl ModelStream for ScriptedStream {
        fn next<'a>(
            &'a mut self,
            cancel: &'a dyn Cancellation,
        ) -> Operation<'a, Option<ModelUpdate>> {
            Box::pin(async move {
                if self.delay_ms > 0 {
                    tokio::select! {
                        biased;
                        () = cancel.cancelled() => {
                            return Err(BoundaryError::new("cancelled", "model stream cancelled"));
                        }
                        () = tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)) => {}
                    }
                }
                if cancel.is_cancelled() {
                    return Err(BoundaryError::new("cancelled", "model stream cancelled"));
                }
                if let Some(thought) = self.thinking.clone()
                    && !self.sent_thinking
                {
                    self.sent_thinking = true;
                    return Ok(Some(ModelUpdate::ThinkingDelta { text: thought }));
                }
                if self.step < self.chunks.len() {
                    let text = self.chunks[self.step].clone();
                    self.step += 1;
                    return Ok(Some(ModelUpdate::TextDelta(text)));
                }
                let full: String = self.chunks.concat();
                Ok(Some(ModelUpdate::Finished {
                    items: vec![HistoryItem::AssistantText(full)],
                    usage: Some(Usage {
                        input_tokens: Some(42),
                        output_tokens: Some(12),
                        cached_input_tokens: None,
                        estimated: false,
                    }),
                }))
            })
        }
    }

    fn test_config_with_workspace(
        home: &std::path::Path,
        workspace: &std::path::Path,
        permissions: &str,
    ) -> config::Config {
        std::fs::create_dir_all(home).unwrap();
        std::fs::create_dir_all(workspace).unwrap();
        let cfg_path = home.join("config.toml");
        std::fs::write(
            &cfg_path,
            format!(
                r#"
schema_version = 1

[defaults]
profile = "fixture"
mode = "work"
permissions = "{permissions}"

[profiles.fixture]
protocol = "openai-chat-completions"
base_url = "http://127.0.0.1:9/v1"
model = "fixture-model"
auth = "none"
context_window_tokens = 400000
max_output_tokens = 4096

[sandbox]
backend = "host"
"#
            ),
        )
        .unwrap();
        config::load(
            Overrides {
                config: Some(cfg_path),
                cwd: Some(workspace.to_path_buf()),
                ..Default::default()
            },
            Environment::from_pairs([("HOME".into(), home.as_os_str().to_owned())]),
            workspace,
        )
        .unwrap()
    }

    fn test_config(root: &std::path::Path) -> config::Config {
        test_config_with_workspace(root, root, "auto")
    }

    struct QueueModel {
        turns: Mutex<VecDeque<Vec<ModelUpdate>>>,
    }

    impl QueueModel {
        fn new(turns: Vec<Vec<ModelUpdate>>) -> Self {
            Self {
                turns: Mutex::new(VecDeque::from(turns)),
            }
        }
    }

    struct VecStream {
        updates: VecDeque<ModelUpdate>,
    }

    impl Model for QueueModel {
        fn start<'a>(
            &'a self,
            request: &'a ModelRequest,
            _cancel: &'a dyn Cancellation,
        ) -> Operation<'a, Box<dyn ModelStream>> {
            let updates = if request.purpose == ModelPurpose::Review {
                VecDeque::from([
                    ModelUpdate::TextDelta(r#"{"decision":"allow","reason":"test"}"#.into()),
                    ModelUpdate::Finished {
                        items: vec![HistoryItem::AssistantText(
                            r#"{"decision":"allow","reason":"test"}"#.into(),
                        )],
                        usage: None,
                    },
                ])
            } else {
                self.turns
                    .lock()
                    .unwrap()
                    .pop_front()
                    .map(VecDeque::from)
                    .unwrap_or_else(|| {
                        VecDeque::from([
                            ModelUpdate::TextDelta("Done".into()),
                            ModelUpdate::Finished {
                                items: vec![HistoryItem::AssistantText("Done".into())],
                                usage: None,
                            },
                        ])
                    })
            };
            Box::pin(async move { Ok(Box::new(VecStream { updates }) as Box<dyn ModelStream>) })
        }
    }

    impl ModelStream for VecStream {
        fn next<'a>(
            &'a mut self,
            cancel: &'a dyn Cancellation,
        ) -> Operation<'a, Option<ModelUpdate>> {
            Box::pin(async move {
                if cancel.is_cancelled() {
                    return Err(BoundaryError::new("cancelled", "stream cancelled"));
                }
                Ok(self.updates.pop_front())
            })
        }
    }

    #[test]
    fn export_specta_bindings() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src/bindings.ts");
        export_bindings_to(&path).expect("specta bindings export should succeed");
        let content = std::fs::read_to_string(&path).expect("bindings.ts should exist");
        for symbol in [
            "listSessions",
            "getBootstrapState",
            "newSession",
            "submitPrompt",
            "cancelRun",
            "compactSession",
            "replyApproval",
            "replyInput",
            "setWorkMode",
            "setPermissionMode",
            "resumeSession",
        ] {
            assert!(content.contains(symbol), "missing {symbol} in bindings.ts");
        }
    }

    #[tokio::test]
    async fn list_sessions_returns_redacted_empty_page_for_fresh_store() {
        let root = tempfile::tempdir().unwrap();
        let cfg = test_config(root.path());
        let state = DesktopState::with_config(cfg);
        let page = state.list_sessions_page(Some(1)).await.unwrap();
        assert_eq!(page.page, 1);
        assert!(page.items.is_empty());
    }

    #[tokio::test]
    async fn submit_prompt_streams_events_and_finishes_via_channel() {
        let root = tempfile::tempdir().unwrap();
        let cfg = test_config(root.path());
        let state = DesktopState::with_config(cfg).with_test_model(Arc::new(|| {
            Arc::new(ScriptedModel {
                chunks: vec!["Hello ".into(), "desktop!".into()],
                thinking: Some("Analyzing prompt".into()),
                delay_ms: 0,
            })
        }));

        let (tx, mut rx) = mpsc::unbounded_channel();
        let res = state
            .submit_prompt("Say hello".into(), vec![], StreamSink::Mpsc(tx))
            .await
            .unwrap();
        assert_eq!(res.action, "started");

        let mut deltas = String::new();
        let mut saw_run_started = false;
        let mut saw_completed = false;
        while let Some(env) = rx.recv().await {
            match env {
                DesktopStreamEnvelope::SchemaEvent(ev) => match ev.payload {
                    crate::types::SchemaEventDto::RunStarted { .. } => saw_run_started = true,
                    crate::types::SchemaEventDto::ModelTextDelta { text, .. } => {
                        deltas.push_str(&text);
                    }
                    _ => {}
                },
                DesktopStreamEnvelope::RunCompleted { status, .. } => {
                    assert_eq!(status, "completed");
                    saw_completed = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(saw_run_started);
        assert!(saw_completed);
        assert_eq!(deltas, "Hello desktop!");

        let bootstrap = state.get_bootstrap_state().await;
        assert!(!bootstrap.run_active);
        assert!(bootstrap.session.is_some());
        let session = bootstrap.session.unwrap();
        assert!(session.messages.iter().any(|m| m.text == "Hello desktop!"));
    }

    #[tokio::test]
    async fn cancel_run_interrupts_streaming_and_cleans_up() {
        let root = tempfile::tempdir().unwrap();
        let cfg = test_config(root.path());
        let state = DesktopState::with_config(cfg).with_test_model(Arc::new(|| {
            Arc::new(ScriptedModel {
                chunks: vec!["chunk1".into(), "chunk2".into(), "chunk3".into()],
                thinking: None,
                delay_ms: 250,
            })
        }));

        let (tx, mut rx) = mpsc::unbounded_channel();
        state
            .submit_prompt("Slow task".into(), vec![], StreamSink::Mpsc(tx))
            .await
            .unwrap();

        // Wait until first event arrives, then cancel.
        let _ = rx.recv().await;
        assert!(state.cancel_run());

        let mut completed_status = None;
        while let Some(env) = rx.recv().await {
            if let DesktopStreamEnvelope::RunCompleted { status, .. } = env {
                completed_status = Some(status);
                break;
            }
        }
        assert_eq!(completed_status.as_deref(), Some("cancelled"));
        assert!(!state.get_bootstrap_state().await.run_active);
    }

    #[tokio::test]
    async fn manual_approval_round_trip_and_mode_switch_invalidation() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        std::fs::write(workspace.join("notes.txt"), "hello desktop").ok();
        let cfg = test_config_with_workspace(&home, &workspace, "manual");
        std::fs::write(workspace.join("notes.txt"), "hello desktop").unwrap();

        let state = DesktopState::with_config(cfg).with_test_model(Arc::new(|| {
            Arc::new(QueueModel::new(vec![
                vec![ModelUpdate::Finished {
                    items: vec![HistoryItem::ToolCall(ToolCall {
                        call_id: "call_todo_1".into(),
                        name: "todo_write".into(),
                        arguments: r#"{"todos":[{"id":"1","content":"Verify approval","status":"in_progress"}]}"#.into(),
                    })],
                    usage: None,
                }],
                vec![ModelUpdate::Finished {
                    items: vec![HistoryItem::ToolCall(ToolCall {
                        call_id: "call_todo_2".into(),
                        name: "todo_write".into(),
                        arguments: r#"{"todos":[{"id":"1","content":"Verify approval","status":"completed"}]}"#.into(),
                    })],
                    usage: None,
                }],
                vec![
                    ModelUpdate::TextDelta("Completed after mode switch".into()),
                    ModelUpdate::Finished {
                        items: vec![HistoryItem::AssistantText(
                            "Completed after mode switch".into(),
                        )],
                        usage: None,
                    },
                ],
            ]))
        }));

        let (tx, mut rx) = mpsc::unbounded_channel();
        state
            .submit_prompt("Read notes".into(), vec![], StreamSink::Mpsc(tx))
            .await
            .unwrap();

        // 1. First approval arrives -> approve via reply_approval.
        let first_approval = loop {
            match rx.recv().await.expect("channel should remain open") {
                DesktopStreamEnvelope::ApprovalNeeded(req) => break req,
                DesktopStreamEnvelope::RunCompleted { error, status, .. } => {
                    panic!("unexpected early run completion ({status}): {error:?}");
                }
                _ => {}
            }
        };
        assert_eq!(first_approval.call_id, "call_todo_1");
        state
            .reply_approval(&first_approval.approval_id, &first_approval.call_id, true)
            .unwrap();

        // 2. Second approval arrives -> switch mode to Plan while pending;
        // pending approval must be invalidated as stale_approval.
        let second_approval = loop {
            match rx.recv().await.expect("channel should remain open") {
                DesktopStreamEnvelope::ApprovalNeeded(req) => break req,
                DesktopStreamEnvelope::RunCompleted { error, status, .. } => {
                    panic!("unexpected early run completion ({status}): {error:?}");
                }
                _ => {}
            }
        };
        let mode_state = state.set_work_mode(WorkModeDto::Plan).await.unwrap();
        assert!(mode_state.active_run);
        assert!(
            mode_state.current == WorkModeDto::Plan
                || mode_state.pending == Some(WorkModeDto::Plan),
            "expected Plan to be current or pending, got {mode_state:?}"
        );
        let stale_err = state
            .reply_approval(&second_approval.approval_id, &second_approval.call_id, true)
            .expect_err("replying to mode-invalidated approval must fail");
        assert_eq!(stale_err.code, "stale_approval");

        let mut completed = None;
        while let Some(env) = rx.recv().await {
            if let DesktopStreamEnvelope::RunCompleted { status, .. } = env {
                completed = Some(status);
                break;
            }
        }
        assert_eq!(completed.as_deref(), Some("completed"));
        assert_eq!(
            state.get_bootstrap_state().await.work_mode,
            WorkModeDto::Plan
        );
    }

    #[tokio::test]
    async fn ask_user_input_round_trip_delivers_answer() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        let cfg = test_config_with_workspace(&home, &workspace, "auto");

        let state = DesktopState::with_config(cfg).with_test_model(Arc::new(|| {
            Arc::new(QueueModel::new(vec![
                vec![ModelUpdate::Finished {
                    items: vec![HistoryItem::ToolCall(ToolCall {
                        call_id: "call_ask".into(),
                        name: "ask_user".into(),
                        arguments:
                            r#"{"question":"Which crate?","options":["soyo-desktop","soyo-cli"]}"#
                                .into(),
                    })],
                    usage: None,
                }],
                vec![
                    ModelUpdate::TextDelta("Chose soyo-desktop.".into()),
                    ModelUpdate::Finished {
                        items: vec![HistoryItem::AssistantText("Chose soyo-desktop.".into())],
                        usage: None,
                    },
                ],
            ]))
        }));

        let (tx, mut rx) = mpsc::unbounded_channel();
        state
            .submit_prompt("Clarify target".into(), vec![], StreamSink::Mpsc(tx))
            .await
            .unwrap();

        let input_req = loop {
            match rx.recv().await.expect("channel should remain open") {
                DesktopStreamEnvelope::InputNeeded(req) => break req,
                DesktopStreamEnvelope::RunCompleted { error, status, .. } => {
                    panic!("unexpected early completion ({status}): {error:?}");
                }
                _ => {}
            }
        };
        assert_eq!(input_req.question, "Which crate?");
        state
            .reply_input(&input_req.input_id, "soyo-desktop".into())
            .unwrap();

        let mut saw_tool_ok = false;
        let mut completed_status = None;
        let mut completed_error = None;
        while let Some(env) = rx.recv().await {
            match env {
                DesktopStreamEnvelope::SchemaEvent(ev) => {
                    if let crate::types::SchemaEventDto::ToolFinished { result, .. } = ev.payload {
                        saw_tool_ok = result.ok && result.formatted.contains("soyo-desktop");
                    }
                }
                DesktopStreamEnvelope::RunCompleted { status, error, .. } => {
                    completed_status = Some(status);
                    completed_error = error;
                    break;
                }
                _ => {}
            }
        }
        assert!(saw_tool_ok);
        assert_eq!(
            completed_status.as_deref(),
            Some("completed"),
            "unexpected completion error: {completed_error:?}"
        );
    }

    #[tokio::test]
    async fn set_permission_mode_and_resume_session_work_when_idle() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        let cfg = test_config_with_workspace(&home, &workspace, "auto");

        let state = DesktopState::with_config(cfg.clone()).with_test_model(Arc::new(|| {
            Arc::new(QueueModel::new(vec![vec![
                ModelUpdate::TextDelta("Saved answer".into()),
                ModelUpdate::Finished {
                    items: vec![HistoryItem::AssistantText("Saved answer".into())],
                    usage: None,
                },
            ]]))
        }));

        let perm = state
            .set_permission_mode(crate::types::PermissionModeDto::Manual)
            .await
            .unwrap();
        assert_eq!(perm, crate::types::PermissionModeDto::Manual);

        let (tx, mut rx) = mpsc::unbounded_channel();
        let submit_res = state
            .submit_prompt("Persist me".into(), vec![], StreamSink::Mpsc(tx))
            .await
            .unwrap();
        while let Some(env) = rx.recv().await {
            if matches!(env, DesktopStreamEnvelope::RunCompleted { .. }) {
                break;
            }
        }
        drop(state);

        // Fresh state resumes the persisted session from SQLite.
        let fresh_state = DesktopState::with_config(cfg);
        let resumed = fresh_state
            .resume_session(&submit_res.session_id, false)
            .await
            .unwrap();
        let session = resumed.bootstrap.session.expect("session should resume");
        assert_eq!(session.id, submit_res.session_id);
        assert!(session.messages.iter().any(|m| m.text == "Saved answer"));
    }
}
