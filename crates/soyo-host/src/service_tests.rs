//! HostService integration tests — moved from the Tauri crate; they exercise
//! the runtime service directly, no socket involved.

#[cfg(test)]
mod tests {
    use crate::service::{ClientOwner, HostService, MAX_CONCURRENT_RUNS, StreamSink};
    use soyo_core::{
        boundary::{Cancellation, Model, ModelRequest, ModelStream, ModelUpdate, Operation},
        config::{self, Environment, Overrides},
        contract::{BoundaryError, HistoryItem, ModelPurpose, ToolCall, Usage},
    };
    use soyo_ipc::types::{
        DesktopStreamEnvelope, ResumeSessionDto, SubmitPromptParams, WorkModeDto,
    };
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };
    use tokio::sync::mpsc;

    fn submit_params(prompt: &str) -> SubmitPromptParams {
        SubmitPromptParams {
            prompt: prompt.into(),
            ..SubmitPromptParams::default()
        }
    }

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

    #[tokio::test]
    async fn list_sessions_returns_redacted_empty_page_for_fresh_store() {
        let root = tempfile::tempdir().unwrap();
        let cfg = test_config(root.path());
        let state = HostService::with_config(cfg);
        let page = state.list_sessions_page(Some(1)).await.unwrap();
        assert_eq!(page.page, 1);
        assert!(page.items.is_empty());
    }

    #[tokio::test]
    async fn submit_prompt_streams_events_and_finishes_via_channel() {
        let root = tempfile::tempdir().unwrap();
        let cfg = test_config(root.path());
        let state = HostService::with_config(cfg).with_test_model(Arc::new(|| {
            Arc::new(ScriptedModel {
                chunks: vec!["Hello ".into(), "desktop!".into()],
                thinking: Some("Analyzing prompt".into()),
                delay_ms: 0,
            })
        }));

        let (tx, mut rx) = mpsc::unbounded_channel();
        let res = state
            .submit_prompt(submit_params("Say hello"), StreamSink::Mpsc(tx))
            .await
            .unwrap();
        assert_eq!(res.action, "started");

        let mut deltas = String::new();
        let mut saw_run_started = false;
        let mut saw_completed = false;
        while let Some(env) = rx.recv().await {
            match env {
                DesktopStreamEnvelope::SchemaEvent(ev) => match ev.payload {
                    soyo_ipc::types::SchemaEventDto::RunStarted { .. } => saw_run_started = true,
                    soyo_ipc::types::SchemaEventDto::ModelTextDelta { text, .. } => {
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
        let state = HostService::with_config(cfg).with_test_model(Arc::new(|| {
            Arc::new(ScriptedModel {
                chunks: vec!["chunk1".into(), "chunk2".into(), "chunk3".into()],
                thinking: None,
                delay_ms: 250,
            })
        }));

        let (tx, mut rx) = mpsc::unbounded_channel();
        state
            .submit_prompt(submit_params("Slow task"), StreamSink::Mpsc(tx))
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

        let state = HostService::with_config(cfg).with_test_model(Arc::new(|| {
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
            .submit_prompt(submit_params("Read notes"), StreamSink::Mpsc(tx))
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

        let state = HostService::with_config(cfg).with_test_model(Arc::new(|| {
            Arc::new(QueueModel::new(vec![
                vec![ModelUpdate::Finished {
                    items: vec![HistoryItem::ToolCall(ToolCall {
                        call_id: "call_ask".into(),
                        name: "ask_user".into(),
                        arguments:
                            r#"{"question":"Which crate?","options":["crate-alpha","crate-beta"]}"#
                                .into(),
                    })],
                    usage: None,
                }],
                vec![
                    ModelUpdate::TextDelta("Chose crate-alpha.".into()),
                    ModelUpdate::Finished {
                        items: vec![HistoryItem::AssistantText("Chose crate-alpha.".into())],
                        usage: None,
                    },
                ],
            ]))
        }));

        let (tx, mut rx) = mpsc::unbounded_channel();
        state
            .submit_prompt(submit_params("Clarify target"), StreamSink::Mpsc(tx))
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
            .reply_input(&input_req.input_id, Some("crate-alpha".into()))
            .unwrap();

        let mut saw_tool_ok = false;
        let mut completed_status = None;
        let mut completed_error = None;
        while let Some(env) = rx.recv().await {
            match env {
                DesktopStreamEnvelope::SchemaEvent(ev) => {
                    if let soyo_ipc::types::SchemaEventDto::ToolFinished { result, .. } = ev.payload
                    {
                        saw_tool_ok = result.ok && result.formatted.contains("crate-alpha");
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

        let state = HostService::with_config(cfg.clone()).with_test_model(Arc::new(|| {
            Arc::new(QueueModel::new(vec![vec![
                ModelUpdate::TextDelta("Saved answer".into()),
                ModelUpdate::Finished {
                    items: vec![HistoryItem::AssistantText("Saved answer".into())],
                    usage: None,
                },
            ]]))
        }));

        let perm = state
            .set_permission_mode(soyo_ipc::types::PermissionModeDto::Manual)
            .await
            .unwrap();
        assert_eq!(perm, soyo_ipc::types::PermissionModeDto::Manual);

        let (tx, mut rx) = mpsc::unbounded_channel();
        let submit_res = state
            .submit_prompt(submit_params("Persist me"), StreamSink::Mpsc(tx))
            .await
            .unwrap();
        while let Some(env) = rx.recv().await {
            if matches!(env, DesktopStreamEnvelope::RunCompleted { .. }) {
                break;
            }
        }
        drop(state);

        // Fresh state resumes the persisted session from SQLite.
        let fresh_state = HostService::with_config(cfg);
        let resumed = fresh_state
            .resume_session(&ResumeSessionDto {
                session_id: submit_res.session_id.clone(),
                accept_config_change: false,
                cwd_explicit: false,
                profile_explicit: false,
                mode_override: None,
            })
            .await
            .unwrap();
        let session = resumed.bootstrap.session.expect("session should resume");
        assert_eq!(session.id, submit_res.session_id);
        assert!(session.messages.iter().any(|m| m.text == "Saved answer"));
    }

    #[tokio::test]
    async fn attachment_protocol_enforces_scope_symlinks_and_safe_mime() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        let cfg = test_config_with_workspace(&home, &workspace, "auto");
        let state = HostService::with_config(cfg);

        // Stage a valid PNG-like file.
        let img_path = workspace.join("sample.png");
        std::fs::write(&img_path, b"\x89PNG\r\n\x1a\n").unwrap();
        let staged = state
            .stage_attachments(vec![img_path.to_string_lossy().into_owned()])
            .await
            .unwrap();
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].mime, "image/png");

        // Protocol serves the staged draft cleanly with strict nosniff headers.
        let ok_resp = crate::attachment::resolve_attachment_request(
            state.attachment_scope.as_ref(),
            state.staged_drafts.as_ref(),
            "GET",
            &staged[0].preview_url,
        );
        assert_eq!(ok_resp.status, 200);
        assert_eq!(ok_resp.content_type, "image/png");

        // Legacy saya-attachment scheme alias resolves staged draft too.
        let legacy_uri = staged[0]
            .preview_url
            .replace("soyo-attachment://", "saya-attachment://");
        let legacy_resp = crate::attachment::resolve_attachment_request(
            state.attachment_scope.as_ref(),
            state.staged_drafts.as_ref(),
            "GET",
            &legacy_uri,
        );
        assert_eq!(legacy_resp.status, 200);

        // Reject path traversal & un-scoped session media requests.
        let traversal = crate::attachment::resolve_attachment_request(
            state.attachment_scope.as_ref(),
            state.staged_drafts.as_ref(),
            "GET",
            "soyo-attachment://localhost/draft/../../etc/passwd",
        );
        assert_eq!(traversal.status, 400);

        let no_session_media = crate::attachment::resolve_attachment_request(
            state.attachment_scope.as_ref(),
            state.staged_drafts.as_ref(),
            "GET",
            "soyo-attachment://localhost/media/01930000-0000-7000-8000-000000000001?ref=soyo-media://obj",
        );
        assert_eq!(no_session_media.status, 403);

        // Reject symlinks during staging.
        #[cfg(unix)]
        {
            let link_path = workspace.join("link.png");
            std::os::unix::fs::symlink(&img_path, &link_path).unwrap();
            let err = state
                .stage_attachments(vec![link_path.to_string_lossy().into_owned()])
                .await
                .unwrap_err();
            assert_eq!(err.code, "attachment_denied");
        }
    }

    #[tokio::test]
    async fn module_toggles_and_settings_update_bootstrap() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        let alt_ws = root.path().join("alt_workspace");
        std::fs::create_dir_all(&alt_ws).unwrap();
        let cfg = test_config_with_workspace(&home, &workspace, "auto");
        let state = HostService::with_config(cfg);

        let modules = state.set_module_enabled("browser", false).await.unwrap();
        let browser = modules.iter().find(|m| m.module == "browser").unwrap();
        assert!(!browser.enabled);
        assert_eq!(browser.source, "module_store");

        let boot = state
            .set_thinking_effort(Some("high".into()))
            .await
            .unwrap();
        assert_eq!(boot.thinking_effort.as_deref(), Some("high"));

        let boot = state
            .set_sandbox_backend(soyo_ipc::types::SandboxBackendDto::Host)
            .await
            .unwrap();
        assert_eq!(boot.sandbox_backend, "host");

        let boot = state.set_autocompact(true, Some(260_000)).await.unwrap();
        assert_eq!(boot.auto_compact_trigger_tokens, Some(260_000));

        let boot = state
            .set_workspace(&alt_ws.to_string_lossy())
            .await
            .unwrap();
        assert_eq!(
            std::path::Path::new(&boot.workspace),
            alt_ws.canonicalize().unwrap()
        );
    }

    #[tokio::test]
    async fn credentials_import_and_revoke_never_expose_secret_values() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let secret_plaintext = "sk-test-secret-token-never-leak-98765";
        let cfg_path = home.join("config.toml");
        std::fs::write(
            &cfg_path,
            r#"
schema_version = 1

[defaults]
profile = "fixture"

[profiles.fixture]
protocol = "openai-chat-completions"
base_url = "http://127.0.0.1:9/v1"
model = "fixture-model"
auth = "none"
context_window_tokens = 400000
max_output_tokens = 4096

[mcp.servers.docs]
enabled = true

[mcp.servers.docs.transport]
type = "http"
endpoint = "https://mcp.example.com/sse"
bearer_env = "MCP_SECRET_TOKEN"
scopes = ["docs:read"]
"#,
        )
        .unwrap();
        let cfg = config::load(
            Overrides {
                config: Some(cfg_path),
                cwd: Some(workspace.clone()),
                ..Default::default()
            },
            Environment::from_pairs([
                ("HOME".into(), home.as_os_str().to_owned()),
                ("MCP_SECRET_TOKEN".into(), secret_plaintext.into()),
            ]),
            &workspace,
        )
        .unwrap();

        let state = HostService::with_config(cfg);
        let imported = state
            .import_credential("docs", "MCP_SECRET_TOKEN", vec!["docs:read".into()], true)
            .await
            .unwrap();
        assert_eq!(imported.server, "docs");
        assert!(!imported.revoked);

        let listed = state.list_credentials().await.unwrap();
        let encoded = serde_json::to_string(&listed).unwrap();
        assert!(!encoded.contains(secret_plaintext));
        assert!(listed.iter().any(|c| c.reference == imported.reference));

        assert!(state.revoke_credential(&imported.reference).await.unwrap());
        let listed_after = state.list_credentials().await.unwrap();
        let revoked_item = listed_after
            .iter()
            .find(|c| c.reference == imported.reference)
            .unwrap();
        assert!(revoked_item.revoked);
    }

    #[tokio::test]
    async fn memory_admin_two_phase_and_session_export_work_end_to_end() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        let cfg = test_config_with_workspace(&home, &workspace, "auto");

        let state = HostService::with_config(cfg).with_test_model(Arc::new(|| {
            Arc::new(QueueModel::new(vec![vec![
                ModelUpdate::TextDelta("Exportable reply".into()),
                ModelUpdate::Finished {
                    items: vec![HistoryItem::AssistantText("Exportable reply".into())],
                    usage: None,
                },
            ]]))
        }));

        // Memory query + two-phase backup export prepare -> confirm.
        let query = state.query_memory("list").await.unwrap();
        assert!(query.data.get("entries").is_some());

        let mem_backup_path = root.path().join("memory-backup.json");
        let prepared = state
            .prepare_memory_action(&format!("export {}", mem_backup_path.display()))
            .await
            .unwrap();
        let confirmed = state
            .confirm_memory_action(&prepared.action_id)
            .await
            .unwrap();
        assert!(confirmed.result.is_object());
        assert!(mem_backup_path.exists());

        // Create and run a session, then export as Markdown and JSON.
        let (tx, mut rx) = mpsc::unbounded_channel();
        let submit = state
            .submit_prompt(submit_params("Run before export"), StreamSink::Mpsc(tx))
            .await
            .unwrap();
        while let Some(env) = rx.recv().await {
            if matches!(env, DesktopStreamEnvelope::RunCompleted { .. }) {
                break;
            }
        }

        let md_path = workspace.join("session-export.md");
        let md_res = state
            .export_session(
                Some(submit.session_id.clone()),
                &md_path.to_string_lossy(),
                soyo_ipc::types::ExportFormatDto::Markdown,
                soyo_ipc::types::ExportScopeDto::Transcript,
                true,
            )
            .await
            .unwrap();
        assert!(md_res.bytes > 0);
        let md_text = std::fs::read_to_string(&md_path).unwrap();
        assert!(md_text.contains("Exportable reply"));
    }

    /// `run_prompt` (`soyo run` over socket): raw schema-v1 envelopes instead
    /// of desktop DTOs, synthetic `session.created` first, `run.finished`
    /// carrying a full `RunResult`, and recovery counts on the response.
    #[tokio::test]
    async fn run_prompt_streams_raw_schema_events_and_finished_result() {
        let root = tempfile::tempdir().unwrap();
        let cfg = test_config(root.path());
        let state = HostService::with_config(cfg).with_test_model(Arc::new(|| {
            Arc::new(ScriptedModel {
                chunks: vec!["Hello ".into(), "socket!".into()],
                thinking: Some("pondering".into()),
                delay_ms: 0,
            })
        }));

        let (tx, mut rx) = mpsc::unbounded_channel();
        let res = state
            .run_prompt(
                soyo_ipc::types::RunPromptParams {
                    prompt: "Say hello".into(),
                    ..Default::default()
                },
                StreamSink::Mpsc(tx),
            )
            .await
            .unwrap();
        assert_eq!(res.action, "started");
        assert_eq!(res.recovered_runs, Some(0));
        assert_eq!(res.recovered_tools, Some(0));

        let mut saw_session_created = false;
        let mut deltas = String::new();
        let mut finished: Option<serde_json::Value> = None;
        let mut completed_status = None;
        let mut event_count = 0usize;
        while let Some(env) = rx.recv().await {
            match env {
                DesktopStreamEnvelope::RawSchemaEvent { event } => {
                    event_count += 1;
                    let parsed: soyo_core::contract::Event =
                        serde_json::from_value(event).expect("valid schema-v1 event");
                    assert_eq!(parsed.schema_version, 1);
                    match parsed.payload {
                        soyo_core::contract::EventPayload::SessionCreated { .. } => {
                            saw_session_created = true;
                        }
                        soyo_core::contract::EventPayload::ModelTextDelta { text, .. } => {
                            deltas.push_str(&text);
                        }
                        soyo_core::contract::EventPayload::RunFinished(result) => {
                            finished = Some(serde_json::to_value(result).unwrap());
                        }
                        _ => {}
                    }
                }
                DesktopStreamEnvelope::RunCompleted { status, .. } => {
                    completed_status = Some(status);
                    break;
                }
                // Raw runs must not leak the desktop event projection.
                // Thinking/Context still flow — interactive clients need the
                // aux channels; headless consumers ignore them.
                DesktopStreamEnvelope::SchemaEvent(_) => {
                    panic!("raw run_prompt must not emit SchemaEvent frames");
                }
                _ => {}
            }
        }
        assert!(saw_session_created, "first event must be session.created");
        assert!(event_count > 2);
        assert_eq!(deltas, "Hello socket!");
        assert_eq!(completed_status.as_deref(), Some("completed"));
        let result = finished.expect("run.finished must arrive as a raw event");
        assert_eq!(result["status"], "completed");
        assert_eq!(result["text"], "Hello socket!");
    }

    /// Sequential `soyo run --session` against a session the helper already
    /// holds prepared must not deadlock on the helper's own session lock —
    /// the held `Prepared` is released before re-resuming the same id.
    #[tokio::test]
    async fn run_prompt_reuses_held_session_lock_for_same_session_id() {
        let root = tempfile::tempdir().unwrap();
        let cfg = test_config(root.path());
        let state = HostService::with_config(cfg).with_test_model(Arc::new(|| {
            Arc::new(ScriptedModel {
                chunks: vec!["ok".into()],
                thinking: None,
                delay_ms: 0,
            })
        }));

        let (tx, mut rx) = mpsc::unbounded_channel();
        let first = state
            .run_prompt(
                soyo_ipc::types::RunPromptParams {
                    prompt: "one".into(),
                    ..Default::default()
                },
                StreamSink::Mpsc(tx),
            )
            .await
            .unwrap();
        while let Some(env) = rx.recv().await {
            if matches!(env, DesktopStreamEnvelope::RunCompleted { .. }) {
                break;
            }
        }

        let (tx, mut rx) = mpsc::unbounded_channel();
        let second = state
            .run_prompt(
                soyo_ipc::types::RunPromptParams {
                    prompt: "two".into(),
                    session_id: Some(first.session_id.clone()),
                    ..Default::default()
                },
                StreamSink::Mpsc(tx),
            )
            .await
            .expect("same-session run must not hit the helper-held lock");
        assert_eq!(second.action, "started");
        assert_eq!(second.session_id, first.session_id);
        let mut completed = None;
        while let Some(env) = rx.recv().await {
            if let DesktopStreamEnvelope::RunCompleted { status, .. } = env {
                completed = Some(status);
                break;
            }
        }
        assert_eq!(completed.as_deref(), Some("completed"));
    }

    /// Headless `run_prompt` (`interactive: false`): approval requests never
    /// reach the wire — the runtime ends `needs_confirmation` like the
    /// in-process CLI without a terminal.
    #[tokio::test]
    async fn run_prompt_headless_ends_needs_confirmation() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        let cfg = test_config_with_workspace(&home, &workspace, "manual");
        let state = HostService::with_config(cfg).with_test_model(Arc::new(|| {
            Arc::new(QueueModel::new(vec![vec![ModelUpdate::Finished {
                items: vec![HistoryItem::ToolCall(ToolCall {
                    call_id: "call_todo_1".into(),
                    name: "todo_write".into(),
                    arguments: r#"{"todos":[{"id":"1","content":"Check","status":"in_progress"}]}"#
                        .into(),
                })],
                usage: None,
            }]]))
        }));

        let (tx, mut rx) = mpsc::unbounded_channel();
        state
            .run_prompt(
                soyo_ipc::types::RunPromptParams {
                    prompt: "Update todos".into(),
                    interactive: false,
                    ..Default::default()
                },
                StreamSink::Mpsc(tx),
            )
            .await
            .unwrap();

        let mut completed_status = None;
        let mut finished_status = None;
        while let Some(env) = rx.recv().await {
            match env {
                DesktopStreamEnvelope::RawSchemaEvent { event } => {
                    let parsed: soyo_core::contract::Event = serde_json::from_value(event).unwrap();
                    if let soyo_core::contract::EventPayload::RunFinished(result) = parsed.payload {
                        finished_status =
                            Some(serde_json::to_value(result.status).unwrap().to_string());
                    }
                }
                DesktopStreamEnvelope::ApprovalNeeded(_)
                | DesktopStreamEnvelope::InputNeeded(_) => {
                    panic!("headless run must not emit interaction requests");
                }
                DesktopStreamEnvelope::RunCompleted { status, .. } => {
                    completed_status = Some(status);
                    break;
                }
                _ => {}
            }
        }
        // RunCompleted.status keeps the existing debug-lowercase convention
        // desktop consumers already parse; the raw run.finished event carries
        // the snake_case RunStatus the CLI maps to exit codes.
        assert_eq!(completed_status.as_deref(), Some("needsconfirmation"));
        assert_eq!(finished_status.as_deref(), Some("\"needs_confirmation\""));
    }

    /// Interactive `run_prompt`: approval requests cross the socket and
    /// `reply_approval` unblocks the run, matching the desktop interaction
    /// contract on the same transport.
    #[tokio::test]
    async fn run_prompt_interactive_approval_round_trip() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        let cfg = test_config_with_workspace(&home, &workspace, "manual");
        let state = HostService::with_config(cfg).with_test_model(Arc::new(|| {
            Arc::new(QueueModel::new(vec![
                vec![ModelUpdate::Finished {
                    items: vec![HistoryItem::ToolCall(ToolCall {
                        call_id: "call_todo_1".into(),
                        name: "todo_write".into(),
                        arguments:
                            r#"{"todos":[{"id":"1","content":"Check","status":"completed"}]}"#
                                .into(),
                    })],
                    usage: None,
                }],
                vec![
                    ModelUpdate::TextDelta("Approved and done".into()),
                    ModelUpdate::Finished {
                        items: vec![HistoryItem::AssistantText("Approved and done".into())],
                        usage: None,
                    },
                ],
            ]))
        }));

        let (tx, mut rx) = mpsc::unbounded_channel();
        state
            .run_prompt(
                soyo_ipc::types::RunPromptParams {
                    prompt: "Update todos".into(),
                    interactive: true,
                    ..Default::default()
                },
                StreamSink::Mpsc(tx),
            )
            .await
            .unwrap();

        let mut completed_status = None;
        while let Some(env) = rx.recv().await {
            match env {
                DesktopStreamEnvelope::ApprovalNeeded(req) => {
                    state
                        .reply_approval(&req.approval_id, &req.call_id, true)
                        .unwrap();
                }
                DesktopStreamEnvelope::RunCompleted { status, .. } => {
                    completed_status = Some(status);
                    break;
                }
                _ => {}
            }
        }
        assert_eq!(completed_status.as_deref(), Some("completed"));
    }

    struct GateModel {
        entered: mpsc::UnboundedSender<tokio::sync::oneshot::Sender<()>>,
    }

    struct GateStream {
        release: Option<tokio::sync::oneshot::Receiver<()>>,
        done: bool,
    }

    impl Model for GateModel {
        fn start<'a>(
            &'a self,
            _request: &'a ModelRequest,
            _cancel: &'a dyn Cancellation,
        ) -> Operation<'a, Box<dyn ModelStream>> {
            let entered = self.entered.clone();
            Box::pin(async move {
                let (tx, rx) = tokio::sync::oneshot::channel();
                let _ = entered.send(tx);
                Ok(Box::new(GateStream {
                    release: Some(rx),
                    done: false,
                }) as Box<dyn ModelStream>)
            })
        }
    }

    impl ModelStream for GateStream {
        fn next<'a>(
            &'a mut self,
            cancel: &'a dyn Cancellation,
        ) -> Operation<'a, Option<ModelUpdate>> {
            Box::pin(async move {
                if self.done {
                    return Ok(None);
                }
                if let Some(rx) = self.release.take() {
                    tokio::select! {
                        biased;
                        () = cancel.cancelled() => {
                            return Err(BoundaryError::new("cancelled", "model stream cancelled"));
                        }
                        _ = rx => {}
                    }
                }
                self.done = true;
                Ok(Some(ModelUpdate::Finished {
                    items: vec![HistoryItem::AssistantText("unblocked".into())],
                    usage: None,
                }))
            })
        }
    }

    async fn wait_completed(rx: &mut mpsc::UnboundedReceiver<DesktopStreamEnvelope>) -> String {
        while let Some(env) = rx.recv().await {
            if let DesktopStreamEnvelope::RunCompleted { status, .. } = env {
                return status;
            }
        }
        panic!("stream ended without RunCompleted");
    }

    #[tokio::test]
    async fn multi_client_runs_isolate_ownership_and_enforce_cap() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        let cfg = test_config_with_workspace(&home, &workspace, "auto");
        let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
        let state = HostService::with_config(cfg).with_test_model(Arc::new(move || {
            Arc::new(GateModel {
                entered: entered_tx.clone(),
            })
        }));

        // Creating a session on conn 2 must not disturb conn 1's active binding (R-15).
        let s1 = state.new_session_for(1, None, None).await.unwrap();
        let _s2_unused = state.new_session_for(2, None, None).await.unwrap();
        let s1_state = state.get_run_state_for(1, None).await.unwrap();
        assert!(!s1_state.active_run);

        let mut sessions = vec![s1.id.clone()];
        for conn in 2..=MAX_CONCURRENT_RUNS + 1 {
            let s = state
                .new_session_for(conn as u64, None, None)
                .await
                .unwrap();
            sessions.push(s.id);
        }

        let mut streams = Vec::new();
        let mut releases = Vec::new();
        for (idx, sid) in sessions.iter().take(MAX_CONCURRENT_RUNS).enumerate() {
            let conn_id = (idx + 1) as u64;
            let (tx, rx) = mpsc::unbounded_channel();
            state
                .submit_prompt_for(
                    ClientOwner {
                        conn_id,
                        request_id: (idx + 10) as u64,
                    },
                    SubmitPromptParams {
                        prompt: format!("run {conn_id}"),
                        session_id: Some(sid.clone()),
                        ..SubmitPromptParams::default()
                    },
                    StreamSink::Mpsc(tx),
                )
                .await
                .unwrap();
            streams.push(rx);
            releases.push(entered_rx.recv().await.unwrap());
        }

        // Cross-connection steer and compaction against conn 1's active session are denied.
        let steer_err = state
            .submit_prompt_for(
                ClientOwner {
                    conn_id: 2,
                    request_id: 99,
                },
                SubmitPromptParams {
                    prompt: "hijack".into(),
                    session_id: Some(sessions[0].clone()),
                    ..SubmitPromptParams::default()
                },
                StreamSink::Mpsc(mpsc::unbounded_channel().0),
            )
            .await
            .unwrap_err();
        assert_eq!(steer_err.code, "session_changed");

        let compact_err = state
            .compact_session_for(
                ClientOwner {
                    conn_id: 2,
                    request_id: 100,
                },
                Some(&sessions[0]),
                false,
                StreamSink::Mpsc(mpsc::unbounded_channel().0),
            )
            .await
            .unwrap_err();
        assert_eq!(compact_err.code, "run_active");

        // 5th concurrent run exceeds MAX_CONCURRENT_RUNS (4).
        let cap_err = state
            .submit_prompt_for(
                ClientOwner {
                    conn_id: (MAX_CONCURRENT_RUNS + 1) as u64,
                    request_id: 101,
                },
                SubmitPromptParams {
                    prompt: "fifth".into(),
                    session_id: Some(sessions[MAX_CONCURRENT_RUNS].clone()),
                    ..SubmitPromptParams::default()
                },
                StreamSink::Mpsc(mpsc::unbounded_channel().0),
            )
            .await
            .unwrap_err();
        assert_eq!(cap_err.code, "run_active");

        for rel in releases {
            let _ = rel.send(());
        }
        for mut rx in streams {
            assert_eq!(wait_completed(&mut rx).await, "completed");
        }
    }

    #[tokio::test]
    async fn cross_connection_cannot_reply_to_other_clients_approval_or_input() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        let cfg = test_config_with_workspace(&home, &workspace, "manual");
        let state = HostService::with_config(cfg).with_test_model(Arc::new(|| {
            Arc::new(QueueModel::new(vec![
                vec![ModelUpdate::Finished {
                    items: vec![HistoryItem::ToolCall(ToolCall {
                        call_id: "call_todo".into(),
                        name: "todo_write".into(),
                        arguments:
                            r#"{"todos":[{"id":"1","content":"Check","status":"completed"}]}"#
                                .into(),
                    })],
                    usage: None,
                }],
                vec![ModelUpdate::Finished {
                    items: vec![HistoryItem::ToolCall(ToolCall {
                        call_id: "call_ask".into(),
                        name: "ask_user".into(),
                        arguments: r#"{"question":"Pick one"}"#.into(),
                    })],
                    usage: None,
                }],
                vec![
                    ModelUpdate::TextDelta("Done".into()),
                    ModelUpdate::Finished {
                        items: vec![HistoryItem::AssistantText("Done".into())],
                        usage: None,
                    },
                ],
            ]))
        }));

        let s1 = state.new_session_for(1, None, None).await.unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        state
            .submit_prompt_for(
                ClientOwner {
                    conn_id: 1,
                    request_id: 1,
                },
                SubmitPromptParams {
                    prompt: "run".into(),
                    session_id: Some(s1.id.clone()),
                    ..SubmitPromptParams::default()
                },
                StreamSink::Mpsc(tx),
            )
            .await
            .unwrap();

        let approval = loop {
            match rx.recv().await.unwrap() {
                DesktopStreamEnvelope::ApprovalNeeded(req) => break req,
                DesktopStreamEnvelope::RunCompleted { status, error, .. } => {
                    panic!("unexpected early completion {status}: {error:?}");
                }
                _ => {}
            }
        };
        let err = state
            .reply_approval_for(2, &approval.approval_id, &approval.call_id, true)
            .unwrap_err();
        assert_eq!(err.code, "unauthorized_client");
        state
            .reply_approval_for(1, &approval.approval_id, &approval.call_id, true)
            .unwrap();

        let input = loop {
            match rx.recv().await.unwrap() {
                DesktopStreamEnvelope::ApprovalNeeded(req) => {
                    state
                        .reply_approval_for(1, &req.approval_id, &req.call_id, true)
                        .unwrap();
                }
                DesktopStreamEnvelope::InputNeeded(req) => break req,
                DesktopStreamEnvelope::RunCompleted { status, error, .. } => {
                    panic!("unexpected early completion {status}: {error:?}");
                }
                _ => {}
            }
        };
        let err = state
            .reply_input_for(2, &input.input_id, Some("answer".into()))
            .unwrap_err();
        assert_eq!(err.code, "unauthorized_client");
        state
            .reply_input_for(1, &input.input_id, Some("answer".into()))
            .unwrap();
        assert_eq!(wait_completed(&mut rx).await, "completed");
    }

    #[tokio::test]
    async fn scoped_cancel_and_disconnect_only_affect_owned_runs() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        let cfg = test_config_with_workspace(&home, &workspace, "auto");
        let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
        let state = HostService::with_config(cfg).with_test_model(Arc::new(move || {
            Arc::new(GateModel {
                entered: entered_tx.clone(),
            })
        }));

        let s1 = state.new_session_for(1, None, None).await.unwrap();
        let s2 = state.new_session_for(2, None, None).await.unwrap();

        let (tx1, mut rx1) = mpsc::unbounded_channel();
        let r1 = state
            .submit_prompt_for(
                ClientOwner {
                    conn_id: 1,
                    request_id: 11,
                },
                SubmitPromptParams {
                    prompt: "first".into(),
                    session_id: Some(s1.id.clone()),
                    ..SubmitPromptParams::default()
                },
                StreamSink::Mpsc(tx1),
            )
            .await
            .unwrap();
        let _rel1 = entered_rx.recv().await.unwrap();

        let (tx2, mut rx2) = mpsc::unbounded_channel();
        let _r2 = state
            .submit_prompt_for(
                ClientOwner {
                    conn_id: 2,
                    request_id: 22,
                },
                SubmitPromptParams {
                    prompt: "second".into(),
                    session_id: Some(s2.id.clone()),
                    ..SubmitPromptParams::default()
                },
                StreamSink::Mpsc(tx2),
            )
            .await
            .unwrap();
        let rel2 = entered_rx.recv().await.unwrap();

        assert!(!state.cancel_request(2, 11));
        assert!(state.cancel_request(1, 11));
        let status1 = wait_completed(&mut rx1).await;
        assert_ne!(status1, "completed");

        let _ = rel2.send(());
        assert_eq!(wait_completed(&mut rx2).await, "completed");

        // Now test cancel_run_for(conn, Some(run_id)) and disconnect isolation.
        let (tx1b, mut rx1b) = mpsc::unbounded_channel();
        let r1b = state
            .submit_prompt_for(
                ClientOwner {
                    conn_id: 1,
                    request_id: 12,
                },
                SubmitPromptParams {
                    prompt: "first again".into(),
                    session_id: Some(s1.id.clone()),
                    ..SubmitPromptParams::default()
                },
                StreamSink::Mpsc(tx1b),
            )
            .await
            .unwrap();
        let _rel1b = entered_rx.recv().await.unwrap();

        let (tx2b, mut rx2b) = mpsc::unbounded_channel();
        let _r2b = state
            .submit_prompt_for(
                ClientOwner {
                    conn_id: 2,
                    request_id: 23,
                },
                SubmitPromptParams {
                    prompt: "second again".into(),
                    session_id: Some(s2.id.clone()),
                    ..SubmitPromptParams::default()
                },
                StreamSink::Mpsc(tx2b),
            )
            .await
            .unwrap();
        let rel2b = entered_rx.recv().await.unwrap();

        assert!(state.cancel_run_for(1, Some(&r1b.run_id)));
        assert_ne!(wait_completed(&mut rx1b).await, "completed");

        // Disconnect conn 1 while conn 2's run is still active: conn 2 completes cleanly.
        state.on_client_disconnected(1).await;
        let _ = rel2b.send(());
        assert_eq!(wait_completed(&mut rx2b).await, "completed");
        let _ = r1.run_id;
    }

    #[tokio::test]
    async fn usage_stats_buckets_runs_by_utc_offset_minutes() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        let cfg = test_config_with_workspace(&home, &workspace, "auto");
        let state = HostService::with_config(cfg).with_test_model(Arc::new(|| {
            Arc::new(ScriptedModel {
                chunks: vec!["ok".into()],
                thinking: None,
                delay_ms: 0,
            })
        }));

        let (tx, mut rx) = mpsc::unbounded_channel();
        state
            .submit_prompt(submit_params("count tokens"), StreamSink::Mpsc(tx))
            .await
            .unwrap();
        assert_eq!(wait_completed(&mut rx).await, "completed");

        let east = state.usage_stats(Some(30), Some(480)).await.unwrap();
        let west = state.usage_stats(Some(30), Some(-480)).await.unwrap();
        assert_eq!(east.days.len(), 1);
        assert_eq!(west.days.len(), 1);
        assert_eq!(east.days[0].day_start.rem_euclid(86_400), 57_600);
        assert_eq!(west.days[0].day_start.rem_euclid(86_400), 28_800);
    }
}
