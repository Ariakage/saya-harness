//! Human decisions require current explicit input. Core retains authorization.
use super::*;
use saya_core::{
    boundary::{Cancellation, Model, ModelRequest, ModelStream, Operation},
    config::{Environment, Overrides},
    contract::{ApprovalRequest, ErrorInfo, PermissionMode, Run},
    input::InputRequest,
    permissions::{PermissionConfig, Policy},
    review::ReviewConfig,
    runtime::{AgentRuntime, RuntimeLimits, RuntimeSetup},
    tools::{
        exec::ExecConfig,
        read::{ReadLimits, ReadTools},
    },
};
use std::{ffi::OsString, time::Duration};
use tokio::sync::{Notify, oneshot};

type ApprovalReceiver = oneshot::Receiver<std::result::Result<Option<bool>, BoundaryError>>;
pub(super) fn fixture() -> (tempfile::TempDir, App) {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let config_path = root.path().join("config.toml");
    std::fs::write(
        &config_path,
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
            config: Some(config_path),
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

#[tokio::test]
async fn copied_notice_is_visible_for_its_bounded_interval() {
    let (_root, mut app) = fixture();
    let now = Instant::now();
    app.show_copy_notice(now);
    assert!(app.state.copied_notice);
    assert!(!app.refresh_copy_notice(now + Duration::from_secs(1)));
    assert!(app.state.copied_notice);
    assert!(app.refresh_copy_notice(now + Duration::from_secs(2)));
    assert!(!app.state.copied_notice);
    assert!(!app.refresh_copy_notice(now + Duration::from_secs(3)));
}
pub(super) fn approval(app: &App, id: &str) -> (Request, ApprovalReceiver) {
    let (reply, receiver) = oneshot::channel();
    (
        Request::Approval(
            ApprovalRequest {
                approval_id: id.into(),
                call_id: "call-fixture".into(),
                run_id: RunId::new(),
                tool: "read_file".into(),
                operation_summary: "Read selected file".into(),
                arguments: serde_json::json!({"path":"fixture.txt"}),
                cwd: app.config.cwd.clone(),
                work_mode: app.config.mode,
            },
            reply,
        ),
        receiver,
    )
}
fn key(code: KeyCode, kind: KeyEventKind) -> KeyEvent {
    KeyEvent::new_with_kind(code, KeyModifiers::NONE, kind)
}

#[test]
fn pasted_files_become_highlightable_tokens_while_model_text_keeps_the_path() {
    let (_root, mut app) = fixture();
    let image = app.config.cwd.join("pasted image.png");
    let file = app.config.cwd.join("notes.md");
    std::fs::write(&image, b"png").unwrap();
    std::fs::write(&file, b"notes").unwrap();
    let pasted = format!("'{}' {}", image.display(), file.display());
    app.insert_paste(&pasted);
    assert_eq!(app.state.editor.text(), "[Image 1] [File 1]");
    assert_eq!(app.draft_attachments.len(), 2);
    let expanded = app.expand_attachments(app.state.editor.text());
    assert!(expanded.contains("[Image 1](<"));
    assert!(expanded.contains("[File 1](<"));
    assert!(expanded.contains(image.to_string_lossy().as_ref()));
    assert!(expanded.contains(file.to_string_lossy().as_ref()));
}

#[tokio::test]
async fn pasted_tokens_move_and_delete_as_atomic_units() {
    let (_root, mut app) = fixture();
    let image = app.config.cwd.join("image.png");
    let file = app.config.cwd.join("notes.md");
    std::fs::write(&image, b"png").unwrap();
    std::fs::write(&file, b"notes").unwrap();
    app.insert_paste(&format!("'{}' '{}'", image.display(), file.display()));
    assert_eq!(app.state.editor.text(), "[Image 1] [File 1]");
    app.state.editor.set_cursor(3);
    app.key(key(KeyCode::Left, KeyEventKind::Press))
        .await
        .unwrap();
    assert_eq!(app.state.editor.cursor(), 0);
    app.state.editor.set_cursor(3);
    app.key(key(KeyCode::Backspace, KeyEventKind::Press))
        .await
        .unwrap();
    assert_eq!(app.state.editor.text(), " [File 1]");
    app.state.editor.set_cursor(3);
    app.key(key(KeyCode::Delete, KeyEventKind::Press))
        .await
        .unwrap();
    assert_eq!(app.state.editor.text(), " ");
}

#[test]
fn clipboard_image_path_inserts_an_image_token() {
    let (root, mut app) = fixture();
    let image = root.path().join(".saya/paste_image/image.png");
    std::fs::create_dir_all(image.parent().unwrap()).unwrap();
    std::fs::write(&image, b"png").unwrap();
    app.insert_image_attachment(&image);
    assert_eq!(app.state.editor.text(), "[Image 1]");
    assert_eq!(app.draft_attachments[0].path, image.to_string_lossy());
    assert_eq!(
        app.draft_attachments[0].kind,
        crate::attachments::Kind::Image
    );
}

#[tokio::test]
async fn markdown_display_commands_change_only_ui_preference_and_keep_pending_approval() {
    let (_root, mut app) = fixture();
    let (request, mut reply) = approval(&app, "display-approval");
    app.request(request);
    let mode = app.config.mode;
    let permissions = app.config.permissions_mode;
    assert!(!app.command("/markdown source").await.unwrap());
    assert_eq!(app.state.markdown_display, view::MarkdownDisplay::Source);
    assert!(app.pending.is_some());
    assert!(matches!(
        reply.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    assert!(
        app.state
            .details
            .as_ref()
            .unwrap()
            .title
            .contains("Approval")
    );
    assert!(!app.command("/markdown render").await.unwrap());
    assert_eq!(app.state.markdown_display, view::MarkdownDisplay::Render);
    assert!(app.command("/markdown unknown").await.is_err());
    assert_eq!(app.state.markdown_display, view::MarkdownDisplay::Render);
    assert!(app.command("/footnote zero").await.is_err());
    assert!(app.command("/footnote 0").await.is_err());
    assert!(app.command("/footnote 1").await.is_err());
    assert!(app.command("/footnote back").await.is_err());
    assert_eq!(app.config.mode, mode);
    assert_eq!(app.config.permissions_mode, permissions);
    assert!(app.active.is_none() && app.prepared.is_none() && app.ui_job.is_none());
    assert!(!app.config.data_dir.exists());
}

#[tokio::test]
async fn runtime_commands_change_sandbox_effort_and_model_without_reusing_approvals() {
    let (_root, mut app) = fixture();
    app.command("/permissions manual").await.unwrap();
    app.command("/sandbox off").await.unwrap();
    assert_eq!(
        app.config.sandbox.backend,
        saya_core::execution::BackendKind::Host
    );
    assert_eq!(app.config.permissions_mode, PermissionMode::Manual);
    app.command("/sandbox on").await.unwrap();
    assert_eq!(
        app.config.sandbox.backend,
        saya_core::execution::BackendKind::Auto
    );
    app.command("/effort high").await.unwrap();
    assert_eq!(
        app.config.profiles["fixture"].thinking_effort,
        Some(saya_core::config::ThinkingEffort::High)
    );
    assert_eq!(app.state.thinking_effort.as_deref(), Some("high"));
    app.command("/thinking default").await.unwrap();
    assert_eq!(app.config.profiles["fixture"].thinking_effort, None);
    assert_eq!(app.state.thinking_effort, None);
    app.command("/autocompact 950k").await.unwrap();
    assert!(app.config.context.auto_compact);
    assert!(
        app.config
            .context
            .trigger_tokens
            .is_some_and(|tokens| tokens < 950_000)
    );
    app.command("/autocompact auto").await.unwrap();
    assert_eq!(app.config.context.trigger_tokens, None);
    assert!(app.config.context.auto_compact);
    app.command("/autocompact off").await.unwrap();
    assert!(!app.config.context.auto_compact);
    let mut other = app.config.profiles["fixture"].clone();
    other.model = "other-model".into();
    app.config.profiles.insert("other".into(), other);
    app.command("/model other").await.unwrap();
    assert_eq!(app.config.profile.as_deref(), Some("other"));
    assert_eq!(app.state.model, "other-model");
    assert_eq!(app.config.permissions_mode, PermissionMode::Manual);
    assert!(app.command("/sandbox maybe").await.is_err());
    assert!(app.command("/effort impossible").await.is_err());
}

#[tokio::test]
async fn ps_opens_a_current_run_process_panel_and_rejects_arguments() {
    let (_root, mut app) = fixture();
    assert!(!app.command("/ps").await.unwrap());
    let details = app.state.details.as_ref().unwrap();
    assert_eq!(details.title, crate::i18n::controller::BACKGROUND_TITLE);
    assert!(
        details
            .lines
            .iter()
            .any(|line| line == crate::i18n::controller::NO_ACTIVE_RUN)
    );
    let error = app.command("/ps now").await.unwrap_err();
    assert_eq!(
        error.to_string(),
        crate::i18n::controller::command_without_arguments("/ps")
    );
    attach_active_call(&mut app).await;
    assert!(!app.command("/ps").await.unwrap());
    assert!(
        app.state
            .details
            .as_ref()
            .unwrap()
            .lines
            .iter()
            .any(|line| line == crate::i18n::controller::NO_BACKGROUND_TASKS)
    );
}

#[test]
fn process_rows_reconstruct_running_cancelling_and_finished_background_jobs() {
    use saya_core::background::{
        BackgroundCancelReason, BackgroundEvent, BackgroundEventKind, BackgroundOutcome,
    };
    let events = vec![
        BackgroundEvent {
            sequence: 1,
            job_id: "job-run".into(),
            kind: BackgroundEventKind::Accepted {
                call_id: "call-run".into(),
                tool: "exec".into(),
            },
        },
        BackgroundEvent {
            sequence: 2,
            job_id: "job-finish".into(),
            kind: BackgroundEventKind::Accepted {
                call_id: "call-finish".into(),
                tool: "read_file".into(),
            },
        },
        BackgroundEvent {
            sequence: 3,
            job_id: "job-cancel".into(),
            kind: BackgroundEventKind::Accepted {
                call_id: "call-cancel".into(),
                tool: "exec".into(),
            },
        },
        BackgroundEvent {
            sequence: 4,
            job_id: "job-cancel".into(),
            kind: BackgroundEventKind::CancellationRequested {
                reason: BackgroundCancelReason::User,
            },
        },
        BackgroundEvent {
            sequence: 5,
            job_id: "job-finish".into(),
            kind: BackgroundEventKind::Finished {
                outcome: BackgroundOutcome::Failed {
                    code: "command_failed".into(),
                },
            },
        },
    ];
    let lines = super::controls::background_lines(&events);
    assert!(lines[0].contains("1 running"));
    assert!(lines[0].contains("3 jobs"));
    assert!(lines.iter().any(|line| line.contains("job-run")));
    assert!(lines.iter().any(|line| line.contains("cancelling")));
    assert!(
        lines
            .iter()
            .any(|line| line.contains("failed · command_failed"))
    );
}

#[tokio::test]
async fn cd_uses_an_absolute_or_relative_directory_and_stays_idle_only() {
    let (root, mut app) = fixture();
    app.record_context_input(Some(Usage {
        input_tokens: Some(1_024),
        output_tokens: Some(64),
        cached_input_tokens: None,
        estimated: false,
    }));
    let nested = app.config.cwd.join("nested");
    std::fs::create_dir(&nested).unwrap();
    app.command("/cd nested").await.unwrap();
    assert_eq!(app.config.cwd, nested.canonicalize().unwrap());
    assert_eq!(app.state.context_input_tokens, Some(1_024));
    assert_eq!(
        app.state.workspace,
        nested.canonicalize().unwrap().to_string_lossy()
    );
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    app.command(&format!("/cd {}", outside.display()))
        .await
        .unwrap();
    assert_eq!(app.config.cwd, outside.canonicalize().unwrap());
    let file = root.path().join("not-a-directory");
    std::fs::write(&file, "x").unwrap();
    assert!(
        app.command(&format!("/cd {}", file.display()))
            .await
            .is_err()
    );
    assert_eq!(app.config.cwd, outside.canonicalize().unwrap());
}

#[tokio::test]
async fn enter_executes_the_selected_completion_without_tab_or_right() {
    let (_root, mut app) = fixture();
    app.state.editor.set_text("/sand");
    app.key(key(KeyCode::Enter, KeyEventKind::Press))
        .await
        .unwrap();
    assert!(app.state.editor.text().is_empty());
    assert!(
        app.state
            .messages
            .iter()
            .any(|message| message.text.contains("Sandbox —"))
    );
    app.state.editor.set_text("/thinking h");
    app.key(key(KeyCode::Enter, KeyEventKind::Press))
        .await
        .unwrap();
    assert_eq!(
        app.config.profiles["fixture"].thinking_effort,
        Some(saya_core::config::ThinkingEffort::High)
    );
}

#[tokio::test]
async fn empty_tab_cycles_all_modes_without_creating_a_conversation_or_notices() {
    let (_root, mut app) = fixture();
    for mode in [
        WorkMode::Code,
        WorkMode::Plan,
        WorkMode::Build,
        WorkMode::Work,
    ] {
        app.key(key(KeyCode::Tab, KeyEventKind::Press))
            .await
            .unwrap();
        assert_eq!(app.config.mode, mode);
        assert_eq!(app.state.mode, format!("{mode:?}").to_uppercase());
        assert!(app.state.editor.text().is_empty());
        assert!(app.state.messages.is_empty());
        assert!(app.prepared.is_none() && app.active.is_none() && app.ui_job.is_none());
    }
    assert!(!app.config.data_dir.exists());
    for kind in [KeyEventKind::Repeat, KeyEventKind::Release] {
        app.key(key(KeyCode::Tab, kind)).await.unwrap();
        assert_eq!(app.config.mode, WorkMode::Work);
        assert!(app.state.editor.text().is_empty());
    }
}

#[tokio::test]
async fn tab_with_a_draft_or_palette_does_not_cycle_the_work_mode() {
    let (_root, mut app) = fixture();
    for (draft, expected) in [
        ("plain", "plain\t"),
        (" ", " \t"),
        ("\n", "\n\t"),
        ("/mo", "/mode plan"),
    ] {
        app.state.editor.set_text(draft);
        app.key(key(KeyCode::Tab, KeyEventKind::Press))
            .await
            .unwrap();
        assert_eq!(app.state.editor.text(), expected);
        assert_eq!(app.config.mode, WorkMode::Work);
    }
    app.state.editor.clear();
    app.state.command_palette = Some(0);
    app.key(key(KeyCode::Tab, KeyEventKind::Press))
        .await
        .unwrap();
    assert_eq!(app.config.mode, WorkMode::Work);
    assert!(app.state.command_palette.is_some());
}

#[tokio::test]
async fn empty_tab_persists_an_idle_session_mode_and_keeps_the_permission_selection() {
    let (_root, mut app) = fixture();
    app.prepared = Some(
        session::create(&app.config, saya_app::new_session(&app.config).unwrap())
            .await
            .unwrap(),
    );
    app.command("/permissions manual").await.unwrap();
    app.key(key(KeyCode::Tab, KeyEventKind::Press))
        .await
        .unwrap();
    assert_eq!(app.config.permissions_mode, PermissionMode::Manual);
    assert_eq!(app.config.mode, WorkMode::Code);
    let prepared = app.prepared.as_ref().unwrap();
    let saved = prepared
        .store
        .load_session(prepared.session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.work_mode, WorkMode::Code);
    assert!(saved.history.is_empty());
}

#[tokio::test]
async fn slash_completion_selects_and_fills_before_enter_executes() {
    let (_root, mut app) = fixture();
    app.state.editor.set_text("/mo");
    app.key(key(KeyCode::Down, KeyEventKind::Press))
        .await
        .unwrap();
    assert_eq!(app.state.editor.text(), "/mo");
    assert_eq!(app.state.completion.current().unwrap().text, "/mode build");
    app.key(key(KeyCode::Tab, KeyEventKind::Press))
        .await
        .unwrap();
    assert_eq!(app.state.editor.text(), "/mode build");
    assert_eq!(app.config.mode, WorkMode::Work);
    assert!(app.active.is_none() && app.prepared.is_none() && app.ui_job.is_none());
    app.key(key(KeyCode::Enter, KeyEventKind::Press))
        .await
        .unwrap();
    assert_eq!(app.config.mode, WorkMode::Build);
    assert!(app.state.editor.text().is_empty());
}

#[tokio::test]
async fn completion_dismissal_preserves_draft_and_tab_explicitly_reopens() {
    let (_root, mut app) = fixture();
    app.state.editor.set_text("/permissions ");
    app.key(key(KeyCode::Down, KeyEventKind::Press))
        .await
        .unwrap();
    app.key(key(KeyCode::Esc, KeyEventKind::Press))
        .await
        .unwrap();
    assert_eq!(app.state.editor.text(), "/permissions ");
    assert!(!app.state.completion.visible());
    app.refresh_completion();
    assert!(!app.state.completion.visible());
    app.key(key(KeyCode::Tab, KeyEventKind::Press))
        .await
        .unwrap();
    assert_eq!(app.state.editor.text(), "/permissions manual");
    assert_eq!(app.config.permissions_mode, PermissionMode::Auto);
    app.key(key(KeyCode::Enter, KeyEventKind::Press))
        .await
        .unwrap();
    assert_eq!(app.config.permissions_mode, PermissionMode::Manual);
}

#[tokio::test]
async fn first_escape_dismisses_completion_before_the_next_escape_denies_approval() {
    for modifiers in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
        let (_root, mut app) = fixture();
        let (request, mut receiver) = approval(&app, "completion-visible");
        app.request(request);
        app.state.editor.set_text("/mo");
        app.key(KeyEvent::new(KeyCode::Esc, modifiers))
            .await
            .unwrap();
        assert!(!app.state.completion.visible());
        assert_eq!(app.state.editor.text(), "/mo");
        assert_eq!(
            app.state.details.as_ref().unwrap().title,
            "Approval required"
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        app.key(key(KeyCode::Esc, KeyEventKind::Press))
            .await
            .unwrap();
        assert_eq!(receiver.await.unwrap().unwrap(), Some(false));
    }
}

#[tokio::test]
async fn completion_respects_the_cursor_literal_slashes_and_normal_editing() {
    let (_root, mut app) = fixture();
    app.state.editor.set_text("/mo");
    app.state.editor.move_left();
    app.key(key(KeyCode::Right, KeyEventKind::Press))
        .await
        .unwrap();
    assert_eq!(app.state.editor.text(), "/mo");
    app.key(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert_eq!(app.state.editor.text(), "/mo");
    app.key(key(KeyCode::Right, KeyEventKind::Press))
        .await
        .unwrap();
    assert_eq!(app.state.editor.text(), "/mode plan");
    for text in ["/unknown", "/exit"] {
        app.state.editor.set_text(text);
        app.key(key(KeyCode::Tab, KeyEventKind::Press))
            .await
            .unwrap();
        assert_eq!(app.state.editor.text(), text);
    }
    for text in ["//literal", "你好🌸\nmore"] {
        app.state.editor.set_text(text);
        app.key(key(KeyCode::Right, KeyEventKind::Press))
            .await
            .unwrap();
        assert_eq!(app.state.editor.text(), text);
        assert!(!app.state.completion.visible());
    }
    app.state.editor.set_text("plain");
    app.key(key(KeyCode::Tab, KeyEventKind::Press))
        .await
        .unwrap();
    assert_eq!(app.state.editor.text(), "plain\t");
}

#[tokio::test]
async fn argument_scaffold_completion_stays_in_the_editor_without_starting_ui_work() {
    let (_root, mut app) = fixture();
    app.state.editor.set_text("/export j");
    app.key(key(KeyCode::Tab, KeyEventKind::Press))
        .await
        .unwrap();
    assert_eq!(app.state.editor.text(), "/export json ");
    assert!(app.prepared.is_none() && app.active.is_none() && app.ui_job.is_none());
    assert!(!app.config.data_dir.exists());
}

#[tokio::test]
async fn command_palette_fills_required_arguments_and_preserves_an_existing_message() {
    let (_root, mut app) = fixture();
    let selected = COMMANDS
        .iter()
        .position(|command| command.text == "/resume ")
        .unwrap();
    app.state.command_palette = Some(selected);
    app.key(key(KeyCode::Enter, KeyEventKind::Press))
        .await
        .unwrap();
    assert_eq!(app.state.editor.text(), "/resume ");
    assert!(app.ui_job.is_none());
    app.state.editor.set_text("草稿 🌸");
    app.state.command_palette = Some(selected);
    app.key(key(KeyCode::Enter, KeyEventKind::Press))
        .await
        .unwrap();
    assert_eq!(app.state.editor.text(), "草稿 🌸");
    assert!(app.ui_job.is_none());
    app.state.editor.clear();
    app.key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert_eq!(app.state.command_palette, Some(0));
    assert!(app.state.details.is_none());
    assert!(!app.state.completion.visible());
    app.key(key(KeyCode::Enter, KeyEventKind::Press))
        .await
        .unwrap();
    assert_eq!(app.config.mode, WorkMode::Plan);
}

#[tokio::test]
async fn approval_decisions_require_a_fresh_press_in_the_visible_prompt() {
    for (decision, expected) in [('y', true), ('n', false)] {
        let (_root, mut app) = fixture();
        let (request, mut receiver) = approval(&app, "current");
        app.request(request);
        app.key(key(KeyCode::Char(decision), KeyEventKind::Repeat))
            .await
            .unwrap();
        app.key(key(KeyCode::Enter, KeyEventKind::Repeat))
            .await
            .unwrap();
        assert!(matches!(
            receiver.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(app.pending.is_some());
        assert!(
            app.state.editor.text().is_empty(),
            "repeat decision entered the editor"
        );
        app.key(key(KeyCode::Char(decision), KeyEventKind::Release))
            .await
            .unwrap();
        assert!(matches!(
            receiver.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        app.key(key(KeyCode::Char(decision), KeyEventKind::Press))
            .await
            .unwrap();
        assert_eq!(receiver.await.unwrap().unwrap(), Some(expected));
        assert!(app.pending.is_none());
    }
}

#[tokio::test]
async fn hidden_approval_is_not_answered_by_the_draft_and_enter() {
    let (_root, mut app) = fixture();
    let (request, mut receiver) = approval(&app, "hidden");
    app.request(request);
    app.command("/help").await.unwrap();
    app.state.editor.set_text("yes");
    app.key(key(KeyCode::Enter, KeyEventKind::Press))
        .await
        .unwrap();
    assert!(matches!(
        receiver.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    assert!(app.pending.is_some());
    app.state.editor.clear();
    app.show_pending();
    app.key(key(KeyCode::Esc, KeyEventKind::Press))
        .await
        .unwrap();
    assert_eq!(receiver.await.unwrap().unwrap(), Some(false));
}

#[tokio::test]
async fn cancel_drops_pending_reply_and_clears_the_draft() {
    let (_root, mut app) = fixture();
    let (request, receiver) = approval(&app, "cancelled");
    app.request(request);
    app.state.editor.set_text("unsubmitted draft");
    app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert!(receiver.await.is_err());
    assert!(app.pending.is_none());
    assert!(app.state.details.is_none());
    assert!(app.state.editor.text().is_empty());
}

#[tokio::test]
async fn clarification_keeps_unicode_and_decodes_literal_slash_escape() {
    for (typed, expected) in [("值 🌸", "值 🌸"), ("//tmp/report", "/tmp/report")] {
        let (_root, mut app) = fixture();
        let (reply, receiver) = oneshot::channel();
        app.request(Request::Input(
            InputRequest {
                question: "Choose a location".into(),
                options: vec![],
            },
            reply,
        ));
        app.state.editor.set_text("  ");
        app.key(key(KeyCode::Enter, KeyEventKind::Press))
            .await
            .unwrap();
        assert!(app.pending.is_some());
        app.state.editor.set_text(typed);
        app.key(key(KeyCode::Enter, KeyEventKind::Press))
            .await
            .unwrap();
        assert_eq!(receiver.await.unwrap().unwrap().as_deref(), Some(expected));
        assert!(app.pending.is_none());
        assert!(app.state.editor.text().is_empty());
    }
}

#[tokio::test]
async fn expired_replies_are_removed_and_closed_arrivals_do_not_replace_current_prompt() {
    let (_root, mut app) = fixture();
    let (current, current_receiver) = approval(&app, "current");
    app.request(current);
    let (expired, expired_receiver) = approval(&app, "expired");
    drop(expired_receiver);
    app.request(expired);
    let Some(Request::Approval(request, _)) = &app.pending else {
        panic!("missing current approval");
    };
    assert_eq!(request.approval_id, "current");
    drop(current_receiver);
    assert!(app.refresh_mode().unwrap());
    assert!(app.pending.is_none());
    assert!(app.state.details.is_none());
}

#[tokio::test]
async fn idle_mode_is_persisted_and_permission_choice_remains_current_invocation_state() {
    let (_root, mut app) = fixture();
    app.prepared = Some(
        session::create(&app.config, saya_app::new_session(&app.config).unwrap())
            .await
            .unwrap(),
    );
    app.command("/permissions manual").await.unwrap();
    app.command("/mode code").await.unwrap();
    assert_eq!(app.config.permissions_mode, PermissionMode::Manual);
    assert_eq!(app.state.permissions, "Manual Approval");
    assert_eq!(app.config.mode, WorkMode::Code);
    assert_eq!(app.state.mode, "CODE");
    assert!(app.state.pending_mode.is_none());
    let prepared = app.prepared.as_ref().unwrap();
    assert_eq!(
        prepared
            .store
            .load_session(prepared.session.id)
            .await
            .unwrap()
            .unwrap()
            .work_mode,
        WorkMode::Code
    );
}

struct HoldingModel(Arc<Notify>);
impl Model for HoldingModel {
    fn start<'a>(
        &'a self,
        _: &'a ModelRequest,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            self.0.notify_one();
            cancel.cancelled().await;
            Err(BoundaryError(
                ErrorInfo {
                    code: "cancelled".into(),
                    message: "fixture cancelled".into(),
                },
                None,
                None,
            ))
        })
    }
}
pub(super) async fn attach_active_call(app: &mut App) {
    let started = Arc::new(Notify::new());
    attach_model(app, Arc::new(HoldingModel(started.clone()))).await;
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .unwrap();
}
async fn attach_model(app: &mut App, model: Arc<dyn Model>) {
    let mut prepared = session::create(&app.config, saya_app::new_session(&app.config).unwrap())
        .await
        .unwrap();
    session::append(&mut prepared, "task".into()).await.unwrap();
    let cancel = Arc::new(RunCancellation::new());
    let files =
        Arc::new(ReadTools::new(&app.config.cwd, &[], false, ReadLimits::default()).unwrap());
    let mut runtime = AgentRuntime::new(RuntimeSetup {
        run: Run {
            id: RunId::new(),
            session_id: prepared.session.id,
            work_mode: WorkMode::Work,
            permissions: app.config.permissions_mode,
        },
        model,
        store: prepared.store.clone(),
        policy: Arc::new(Policy::new(&app.config.cwd, PermissionConfig::default()).unwrap()),
        files,
        cancellation: cancel.clone(),
        review: ReviewConfig::new("fixture".into()),
        exec: ExecConfig::default(),
        preauthorizations: Preauthorizations::default(),
        known_secrets: vec![],
        summary: "task".into(),
        confirmation: None,
        limits: RuntimeLimits::default(),
    })
    .unwrap();
    let handle = runtime.handle();
    let mut current = prepared.session.clone();
    let join = tokio::spawn(async move {
        let result = runtime.run(&mut current).await;
        (result, current)
    });
    let (_sender, events) = mpsc::channel(1);
    let (_context_sender, context) = watch::channel(None);
    let (_thinking_sender, thinking) = watch::channel(ThinkingLog::default());
    let (_interaction, requests) = input_channel(1).unwrap();
    app.active = Some(Active {
        kind: RunKind::Execution,
        cancel,
        join,
        events,
        events_open: false,
        context,
        thinking,
        thinking_ids: HashMap::new(),
        execution_request: None,
        assistant_id: None,
        tool_cards: HashMap::new(),
        handle,
        requests,
        requests_open: false,
        summary: turn_summary::TurnSummary::new("fixture".into()),
    });
    app.prepared = Some(prepared);
}
pub(super) async fn stop_active_call(app: &mut App) {
    app.cancel();
    let active = app.active.as_mut().unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(5), &mut active.join)
        .await
        .unwrap();
    app.finish(outcome).await.unwrap();
}

#[tokio::test]
async fn changing_mode_drops_old_approval_and_shows_actual_and_pending_core_modes() {
    let (_root, mut app) = fixture();
    attach_active_call(&mut app).await;
    let (request, receiver) = approval(&app, "old-policy");
    app.request(request);
    app.command("/mode plan").await.unwrap();
    assert_eq!(
        receiver.await.unwrap().unwrap_err().0.code,
        "stale_approval"
    );
    assert!(app.pending.is_none());
    assert_eq!(app.state.mode, "WORK");
    assert_eq!(app.state.pending_mode.as_deref(), Some("PLAN"));
    let core = app.active.as_ref().unwrap().handle.mode().unwrap();
    assert_eq!(core.current, WorkMode::Work);
    assert_eq!(core.pending, Some(WorkMode::Plan));
    stop_active_call(&mut app).await;
}

#[tokio::test]
async fn repeated_empty_tab_uses_the_latest_requested_mode_and_invalidates_old_approval() {
    let (_root, mut app) = fixture();
    attach_active_call(&mut app).await;
    app.key(key(KeyCode::Tab, KeyEventKind::Press))
        .await
        .unwrap();
    assert_eq!(app.state.pending_mode.as_deref(), Some("CODE"));
    let (request, receiver) = approval(&app, "tab-old-policy");
    app.request(request);
    app.key(key(KeyCode::Tab, KeyEventKind::Press))
        .await
        .unwrap();
    assert_eq!(
        receiver.await.unwrap().unwrap_err().0.code,
        "stale_approval"
    );
    assert!(app.pending.is_none());
    assert_eq!(app.state.pending_mode.as_deref(), Some("PLAN"));
    for expected in [WorkMode::Build, WorkMode::Work] {
        app.key(key(KeyCode::Tab, KeyEventKind::Press))
            .await
            .unwrap();
        let snapshot = app.active.as_ref().unwrap().handle.mode().unwrap();
        assert_eq!(snapshot.current, WorkMode::Work);
        assert_eq!(snapshot.pending.unwrap_or(snapshot.current), expected);
        assert_eq!(app.config.mode, WorkMode::Work);
    }
    assert!(app.state.editor.text().is_empty());
    stop_active_call(&mut app).await;
}

#[tokio::test]
async fn execution_rounds_follow_their_tool_results_in_the_visible_conversation() {
    let (_root, mut app) = fixture();
    attach_active_call(&mut app).await;
    let id = app.prepared.as_ref().unwrap().session.id;
    let event = |payload| Event {
        schema_version: 1,
        session_id: id,
        run_id: None,
        seq: 1,
        timestamp: "fixture".into(),
        payload,
    };
    app.event(event(EventPayload::ModelStarted {
        request_id: "first".into(),
        purpose: ModelPurpose::Execution,
        profile: "fixture".into(),
    }));
    app.event(event(EventPayload::ModelTextDelta {
        request_id: "first".into(),
        text: "Before the tool".into(),
    }));
    app.event(event(EventPayload::ToolStarted {
        call_id: "read".into(),
        name: "read_file".into(),
    }));
    app.event(event(EventPayload::ToolFinished {
        call_id: "read".into(),
        name: "read_file".into(),
        result: saya_core::contract::ToolResult {
            ok: true,
            data: Some(serde_json::json!({"text":"tool result"})),
            error: None,
            truncated: false,
        },
    }));
    app.event(event(EventPayload::ModelStarted {
        request_id: "final".into(),
        purpose: ModelPurpose::Execution,
        profile: "fixture".into(),
    }));
    app.event(event(EventPayload::ModelTextDelta {
        request_id: "final".into(),
        text: "After the tool".into(),
    }));
    let last = app.state.messages.back().unwrap();
    assert_eq!(last.role, Role::Assistant);
    assert_eq!(last.text, "After the tool");
    assert!(
        app.state
            .messages
            .iter()
            .any(|message| { message.text == "Before the tool" && !message.streaming })
    );
    stop_active_call(&mut app).await;
}

#[tokio::test]
async fn context_meter_tracks_only_the_latest_execution_request_and_clears_unknown_usage() {
    let (_root, mut app) = fixture();
    let session_id = saya_core::contract::SessionId::new();
    let event = |payload| Event {
        schema_version: 1,
        session_id,
        run_id: None,
        seq: 1,
        timestamp: "fixture".into(),
        payload,
    };
    for (purpose, tokens, expected) in [
        (ModelPurpose::Execution, Some(1024), Some(1024)),
        (ModelPurpose::Review, Some(999), Some(1024)),
        (ModelPurpose::Execution, Some(512), Some(512)),
        (ModelPurpose::Execution, None, None),
    ] {
        app.event(event(EventPayload::ModelFinished {
            request_id: "fixture".into(),
            purpose,
            ok: true,
            usage: tokens.map(|tokens| Usage {
                input_tokens: Some(tokens),
                output_tokens: Some(50),
                cached_input_tokens: None,
                estimated: true,
            }),
            error: None,
        }));
        assert_eq!(app.state.context_input_tokens, expected);
        assert_eq!(app.state.context_estimated, expected.is_some());
    }
    app.record_context_input(Some(Usage {
        input_tokens: Some(100),
        output_tokens: None,
        cached_input_tokens: None,
        estimated: false,
    }));
    for purpose in [ModelPurpose::Review, ModelPurpose::Execution] {
        app.event(event(EventPayload::ModelStarted {
            request_id: "fixture".into(),
            purpose,
            profile: "fixture".into(),
        }));
        assert_eq!(
            app.state.context_input_tokens,
            (purpose == ModelPurpose::Review).then_some(100)
        );
    }
}

#[tokio::test]
async fn final_event_drain_keeps_latest_request_usage_separate_from_run_totals() {
    let (_root, mut app) = fixture();
    attach_active_call(&mut app).await;
    let session_id = app.prepared.as_ref().unwrap().session.id;
    let (sender, events) = mpsc::channel(2);
    app.active.as_mut().unwrap().events = events;
    for (purpose, input) in [(ModelPurpose::Execution, 512), (ModelPurpose::Review, 777)] {
        sender
            .send(Event {
                schema_version: 1,
                session_id,
                run_id: None,
                seq: 1,
                timestamp: "fixture".into(),
                payload: EventPayload::ModelFinished {
                    request_id: "queued".into(),
                    purpose,
                    ok: true,
                    usage: Some(Usage {
                        input_tokens: Some(input),
                        output_tokens: Some(10),
                        cached_input_tokens: None,
                        estimated: false,
                    }),
                    error: None,
                },
            })
            .await
            .unwrap();
    }
    app.cancel();
    let (result, session) = (&mut app.active.as_mut().unwrap().join).await.unwrap();
    let mut result = result.unwrap();
    result.usage = Some(Usage {
        input_tokens: Some(9000),
        output_tokens: Some(2000),
        cached_input_tokens: None,
        estimated: false,
    });
    app.finish(Ok((Ok(result), session))).await.unwrap();
    assert_eq!(app.state.context_input_tokens, Some(512));
    assert!(!app.state.context_estimated);
    assert!(app.state.usage.contains("9000"));
    let summary = app.state.messages.back().unwrap();
    assert_eq!(summary.role, Role::RunSummary);
    assert!(summary.text.ends_with("11k tokens"));
    assert!(summary.text.contains("— tok/s"));
}

#[tokio::test]
async fn run_summaries_close_every_terminal_status_once_with_actual_mode_and_totals() {
    use saya_core::contract::RunStatus;
    for status in [
        RunStatus::Completed,
        RunStatus::Failed,
        RunStatus::Cancelled,
        RunStatus::NeedsConfirmation,
        RunStatus::NeedsInput,
        RunStatus::LimitReached,
    ] {
        let (_root, mut app) = fixture();
        attach_active_call(&mut app).await;
        app.state.pending_mode = Some("PLAN".into());
        app.cancel();
        let (result, mut session) = (&mut app.active.as_mut().unwrap().join).await.unwrap();
        let mut result = result.unwrap();
        result.status = status;
        result.text = Some("retained final response".into());
        result.usage = Some(Usage {
            input_tokens: Some(500),
            output_tokens: Some(100),
            cached_input_tokens: None,
            estimated: false,
        });
        session.work_mode = WorkMode::Code;
        let repeated = (Ok(result.clone()), session.clone());
        let history = session.history.clone();
        app.finish(Ok((Ok(result), session))).await.unwrap();
        let summary = app.state.messages.back().unwrap();
        assert_eq!(summary.role, Role::RunSummary);
        assert_eq!(summary.label, "CODE");
        assert!(summary.text.starts_with("fixture · "));
        assert!(summary.text.ends_with("— tok/s · 600 tokens"));
        assert!(!app.state.activity_active);
        assert_eq!(app.prepared.as_ref().unwrap().session.history, history);
        assert!(app.finish(Ok(repeated)).await.is_err());
        assert_eq!(
            app.state
                .messages
                .iter()
                .filter(|message| message.role == Role::RunSummary)
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn run_summary_model_metadata_uses_refreshed_credential_redaction() {
    let (_root, mut app) = fixture();
    attach_active_call(&mut app).await;
    app.active.as_mut().unwrap().summary =
        turn_summary::TurnSummary::new("fixture-synthetic-late-token-model".into());
    app.secret_registry
        .register(&["synthetic-late-token".into()])
        .unwrap();
    stop_active_call(&mut app).await;
    let summary = app.state.messages.back().unwrap();
    assert_eq!(summary.role, Role::RunSummary);
    assert!(summary.text.contains("[REDACTED]"));
    assert!(!summary.text.contains("synthetic-late-token"));
}

#[tokio::test]
async fn summary_usage_and_model_timing_do_not_leak_between_turns() {
    let (_root, mut app) = fixture();
    for usage in [
        Some(Usage {
            input_tokens: Some(500),
            output_tokens: Some(100),
            cached_input_tokens: None,
            estimated: false,
        }),
        None,
    ] {
        attach_active_call(&mut app).await;
        app.cancel();
        let (result, session) = (&mut app.active.as_mut().unwrap().join).await.unwrap();
        let mut result = result.unwrap();
        result.usage = usage;
        app.finish(Ok((Ok(result), session))).await.unwrap();
    }
    let summaries = app
        .state
        .messages
        .iter()
        .filter(|message| message.role == Role::RunSummary)
        .collect::<Vec<_>>();
    assert_eq!(summaries.len(), 2);
    assert!(summaries[0].text.ends_with("— tok/s · 600 tokens"));
    assert!(summaries[1].text.ends_with("— tok/s · — tokens"));
}

#[tokio::test]
async fn activity_ticks_only_change_visible_frames_and_each_run_starts_at_zero() {
    let (_root, mut app) = fixture();
    attach_active_call(&mut app).await;
    let now = Instant::now();
    app.active.as_mut().unwrap().summary.start(now);
    app.state.status = "Running".into();
    assert!(app.refresh_activity(now));
    assert_eq!(app.state.activity_frame, 0);
    assert!(!app.refresh_activity(now + Duration::from_millis(119)));
    assert!(app.refresh_activity(now + Duration::from_millis(120)));
    assert_eq!(app.state.activity_frame, 1);
    for status in ["Awaiting approval", "Awaiting input"] {
        app.state.status = status.into();
        assert!(!app.refresh_activity(now + Duration::from_millis(500)));
        assert_eq!(app.state.activity_frame, 1);
    }
    app.state.status = "Thinking".into();
    assert!(app.refresh_activity(now + Duration::from_millis(600)));
    assert_eq!(app.state.activity_frame, 5);
    app.cancel();
    assert!(app.refresh_activity(now + Duration::from_millis(720)));
    assert_eq!(app.state.activity_frame, 6);
    stop_active_call(&mut app).await;
    assert!(!app.state.activity_active);
    assert_eq!(app.state.activity_frame, 0);
    assert!(!app.refresh_activity(now + Duration::from_secs(99)));
    attach_active_call(&mut app).await;
    let next = Instant::now();
    app.active.as_mut().unwrap().summary.start(next);
    app.state.status = "Running".into();
    assert!(app.refresh_activity(next));
    assert_eq!(app.state.activity_frame, 0);
    stop_active_call(&mut app).await;
}

#[tokio::test]
async fn execution_start_clears_old_usage_even_before_expired_approval_is_removed() {
    for expired in [false, true] {
        let (_root, mut app) = fixture();
        app.record_context_input(Some(Usage {
            input_tokens: Some(512),
            output_tokens: Some(10),
            cached_input_tokens: None,
            estimated: true,
        }));
        let (request, receiver) = approval(&app, "meter-waiting");
        app.request(request);
        if expired {
            drop(receiver);
        }
        app.event(Event {
            schema_version: 1,
            session_id: saya_core::contract::SessionId::new(),
            run_id: None,
            seq: 1,
            timestamp: "fixture".into(),
            payload: EventPayload::ModelStarted {
                request_id: "next".into(),
                purpose: ModelPurpose::Execution,
                profile: "fixture".into(),
            },
        });
        assert_eq!(app.state.context_input_tokens, None);
        assert!(!app.state.context_estimated);
        assert_eq!(app.state.status, "Awaiting approval");
    }
}

#[tokio::test]
async fn final_event_drain_clears_old_usage_when_only_the_next_request_start_arrived() {
    let (_root, mut app) = fixture();
    attach_active_call(&mut app).await;
    app.record_context_input(Some(Usage {
        input_tokens: Some(512),
        output_tokens: Some(10),
        cached_input_tokens: None,
        estimated: false,
    }));
    let (sender, events) = mpsc::channel(1);
    app.active.as_mut().unwrap().events = events;
    sender
        .send(Event {
            schema_version: 1,
            session_id: app.prepared.as_ref().unwrap().session.id,
            run_id: None,
            seq: 1,
            timestamp: "fixture".into(),
            payload: EventPayload::ModelStarted {
                request_id: "next-without-usage".into(),
                purpose: ModelPurpose::Execution,
                profile: "fixture".into(),
            },
        })
        .await
        .unwrap();
    stop_active_call(&mut app).await;
    assert_eq!(app.state.context_input_tokens, None);
    assert!(!app.state.context_estimated);
}

#[tokio::test]
async fn context_snapshots_refresh_and_the_final_closed_channel_preserves_the_latest() {
    let (_root, mut app) = fixture();
    attach_active_call(&mut app).await;
    let (sender, receiver) = watch::channel(None);
    app.active.as_mut().unwrap().context = receiver;
    sender.send_replace(Some(ContextSnapshot {
        entries: vec![saya_app::context::ContextEntry {
            label: "System",
            tokens: 120,
        }],
        skills_count: Some(2),
    }));
    assert!(app.refresh_context());
    assert_eq!(app.state.context_breakdown[0].label, "System");
    assert_eq!(app.state.context_breakdown[0].tokens, 120);
    assert_eq!(app.state.skills_count, Some(2));
    assert!(app.state.context_breakdown_estimated);
    assert!(!app.refresh_context());
    sender.send_replace(Some(ContextSnapshot {
        entries: vec![saya_app::context::ContextEntry {
            label: "Conversation",
            tokens: 300,
        }],
        skills_count: Some(0),
    }));
    drop(sender);
    stop_active_call(&mut app).await;
    assert_eq!(app.state.context_breakdown[0].label, "Conversation");
    assert_eq!(app.state.context_breakdown[0].tokens, 300);
    assert_eq!(app.state.skills_count, Some(0));
}

#[tokio::test]
async fn one_line_details_page_through_each_row() {
    let (_root, mut app) = fixture();
    let (request, receiver) = approval(&app, "small-viewport");
    app.request(request);
    app.state.details_page_rows = 1;
    app.key(key(KeyCode::PageDown, KeyEventKind::Press))
        .await
        .unwrap();
    assert_eq!(app.state.details_scroll, 1);
    app.key(key(KeyCode::PageDown, KeyEventKind::Press))
        .await
        .unwrap();
    assert_eq!(app.state.details_scroll, 2);
    app.key(key(KeyCode::PageUp, KeyEventKind::Press))
        .await
        .unwrap();
    assert_eq!(app.state.details_scroll, 1);
    app.reply_approval(false);
    assert_eq!(receiver.await.unwrap().unwrap(), Some(false));
}

#[tokio::test]
async fn deferred_mode_blocks_old_decisions_until_ui_cleanup_and_core_invalidation() {
    let (_root, mut app) = fixture();
    attach_active_call(&mut app).await;
    let (request, mut decision) = approval(&app, "old-policy");
    app.request(request);
    let (sender, receiver) = oneshot::channel();
    app.ui_job = Some(super::services::UiJob {
        cancel: Arc::new(RunCancellation::new()),
        join: tokio::spawn(async move { Ok(receiver.await.unwrap()) }),
    });
    app.command("/mode plan").await.unwrap();
    app.state.editor.set_text("yes");
    app.submit().await.unwrap();
    assert!(matches!(
        decision.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    assert!(app.pending.is_some());
    assert_eq!(
        app.active.as_ref().unwrap().handle.mode().unwrap().current,
        WorkMode::Work
    );
    assert!(
        sender
            .send(super::services::UiResult::Details(crate::view::Details {
                title: "cancelled query".into(),
                lines: vec![],
            }))
            .is_ok()
    );
    let result = super::services::next_job(&mut app.ui_job).await;
    app.finish_ui(result).await.unwrap();
    assert_eq!(
        decision.await.unwrap().unwrap_err().0.code,
        "stale_approval"
    );
    assert_eq!(
        app.active.as_ref().unwrap().handle.mode().unwrap().pending,
        Some(WorkMode::Plan)
    );
    stop_active_call(&mut app).await;
}

#[tokio::test]
async fn pending_mode_change_prevents_starting_a_workspace_export() {
    let (_root, mut app) = fixture();
    attach_active_call(&mut app).await;
    app.command("/mode plan").await.unwrap();
    for command in ["/export", "/export-full"] {
        let error = app
            .service(command, "json forbidden.json")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("pending mode change"));
        assert!(app.ui_job.is_none());
        assert!(!app.config.cwd.join("forbidden.json").exists());
    }
    stop_active_call(&mut app).await;
}

#[tokio::test]
async fn active_permission_change_is_refused_and_cancellation_reaches_owned_runtime() {
    let (_root, mut app) = fixture();
    attach_active_call(&mut app).await;
    assert!(app.command("/permissions bypass").await.is_err());
    assert_eq!(app.config.permissions_mode, PermissionMode::Auto);
    let cancel = app.active.as_ref().unwrap().cancel.clone();
    let (request, receiver) = approval(&app, "cancelled-active");
    app.request(request);
    app.cancel();
    assert!(cancel.is_cancelled());
    assert!(receiver.await.is_err());
    stop_active_call(&mut app).await;
}

#[tokio::test]
async fn active_run_accepts_bounded_guidance_without_replacing_the_original_task() {
    let (_root, mut app) = fixture();
    attach_active_call(&mut app).await;
    app.state
        .editor
        .set_text("Prioritize the security regression tests");
    app.key(key(KeyCode::Enter, KeyEventKind::Press))
        .await
        .unwrap();
    assert!(app.state.editor.text().is_empty());
    assert!(
        app.state
            .messages
            .iter()
            .any(|message| message.role == Role::User
                && message.text == "Prioritize the security regression tests")
    );
    assert!(
        app.state
            .messages
            .iter()
            .any(|message| message.text.contains("Guidance queued"))
    );
    assert!(app.active.is_some());
    stop_active_call(&mut app).await;
}

#[tokio::test]
async fn sandbox_status_is_read_only_during_an_active_run_but_switches_are_idle_only() {
    let (_root, mut app) = fixture();
    attach_active_call(&mut app).await;
    app.command("/sandbox status").await.unwrap();
    assert!(
        app.state
            .messages
            .iter()
            .any(|message| message.text.contains("Sandbox —"))
    );
    assert!(app.command("/sandbox off").await.is_err());
    assert_eq!(
        app.config.sandbox.backend,
        saya_core::execution::BackendKind::Auto
    );
    stop_active_call(&mut app).await;
}

#[tokio::test]
async fn plan_storage_checks_apply_to_every_permission_mode_before_creating_files() {
    for permissions in [
        PermissionMode::Auto,
        PermissionMode::Manual,
        PermissionMode::Bypass,
    ] {
        let (_root, mut app) = fixture();
        app.config.permissions_mode = permissions;
        app.config.data_dir = app.config.cwd.join("data");
        assert!(app.command("/mode plan").await.is_err());
        assert_eq!(app.config.mode, WorkMode::Work);
        assert_eq!(app.state.mode, "WORK");
        assert!(!app.config.data_dir.exists());
    }
}

#[tokio::test]
async fn modified_editing_shortcuts_do_not_grant_approval() {
    for modifiers in [
        KeyModifiers::CONTROL,
        KeyModifiers::ALT,
        KeyModifiers::SUPER,
    ] {
        let (_root, mut app) = fixture();
        let (request, mut receiver) = approval(&app, "modifier");
        app.request(request);
        app.key(KeyEvent::new(KeyCode::Char('y'), modifiers))
            .await
            .unwrap();
        assert!(
            matches!(
                receiver.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ),
            "modifier {modifiers:?} granted approval"
        );
        assert!(app.pending.is_some());
    }
    let (_root, mut app) = fixture();
    let (request, receiver) = approval(&app, "uppercase");
    app.request(request);
    app.key(KeyEvent::new(KeyCode::Char('Y'), KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert_eq!(receiver.await.unwrap().unwrap(), Some(true));
}

struct BackgroundThenPanic {
    marker: std::path::PathBuf,
    requests: std::sync::atomic::AtomicUsize,
}
struct BackgroundLaunch(Option<Vec<HistoryItem>>);
impl ModelStream for BackgroundLaunch {
    fn next<'a>(
        &'a mut self,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Option<saya_core::boundary::ModelUpdate>> {
        Box::pin(async move {
            Ok(self
                .0
                .take()
                .map(|items| saya_core::boundary::ModelUpdate::Finished { items, usage: None }))
        })
    }
}
impl Model for BackgroundThenPanic {
    fn start<'a>(
        &'a self,
        _: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            if self
                .requests
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                return Ok(Box::new(BackgroundLaunch(Some(vec![HistoryItem::ToolCall(
                    saya_core::contract::ToolCall {
                        call_id: "launch-before-panic".into(),
                        name: "background_start".into(),
                        arguments: serde_json::json!({
                            "tool": "exec",
                            "arguments": {"argv": [
                                "/bin/sh", "-c",
                                "printf '%s\\n' \"$$\" > \"$1\"; exec /bin/sleep 30",
                                "sh", self.marker
                            ]}
                        })
                        .to_string(),
                    },
                )]))) as Box<dyn ModelStream>);
            }
            tokio::time::timeout(Duration::from_secs(3), async {
                while !self.marker.exists() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            panic!("fixture runtime panic with an active background process");
        })
    }
}

struct BackgroundHolding {
    requests: std::sync::atomic::AtomicUsize,
    started: Arc<Notify>,
}

impl Model for BackgroundHolding {
    fn start<'a>(
        &'a self,
        _: &'a ModelRequest,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            if self
                .requests
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                self.started.notify_one();
                return Ok(Box::new(BackgroundLaunch(Some(vec![HistoryItem::ToolCall(
                    saya_core::contract::ToolCall {
                        call_id: "ps-background-start".into(),
                        name: "background_start".into(),
                        arguments: serde_json::json!({
                            "tool": "exec",
                            "arguments": {
                                "argv": ["/bin/sleep", "30"],
                                "timeout_seconds": 30
                            }
                        })
                        .to_string(),
                    },
                )]))) as Box<dyn ModelStream>);
            }
            cancel.cancelled().await;
            Err(BoundaryError(
                ErrorInfo {
                    code: "cancelled".into(),
                    message: "fixture cancelled".into(),
                },
                None,
                None,
            ))
        })
    }
}

#[tokio::test]
async fn ps_lists_a_live_background_job_from_the_current_run() {
    let (_root, mut app) = fixture();
    app.config.permissions_mode = PermissionMode::Bypass;
    let model = Arc::new(BackgroundHolding {
        requests: std::sync::atomic::AtomicUsize::new(0),
        started: Arc::new(Notify::new()),
    });
    attach_model(&mut app, model.clone()).await;
    tokio::time::timeout(Duration::from_secs(5), model.started.notified())
        .await
        .unwrap();
    let job_id = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(id) = app
                .active
                .as_ref()
                .unwrap()
                .handle
                .background_events()
                .unwrap()
                .into_iter()
                .find_map(|event| {
                    matches!(
                        event.kind,
                        saya_core::background::BackgroundEventKind::Accepted { .. }
                    )
                    .then_some(event.job_id)
                })
            {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(!app.command("/ps").await.unwrap());
    let lines = &app.state.details.as_ref().unwrap().lines;
    assert!(lines.iter().any(|line| line == &job_id));
    assert!(lines.iter().any(|line| line.trim() == "running"));
    app.cancel();
    let active = app.active.as_mut().unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(10), &mut active.join)
        .await
        .unwrap();
    app.finish(outcome).await.unwrap();
}

#[tokio::test]
async fn terminal_error_teardown_awaits_background_cleanup_after_runtime_panic() {
    let (root, mut app) = fixture();
    let marker = root.path().join("background.pid");
    app.config.permissions_mode = PermissionMode::Bypass;
    attach_model(
        &mut app,
        Arc::new(BackgroundThenPanic {
            marker: marker.clone(),
            requests: std::sync::atomic::AtomicUsize::new(0),
        }),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !app.active.as_ref().unwrap().join.is_finished() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let pid: i32 = std::fs::read_to_string(&marker)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SAFETY: signal zero only probes the owned fixture process.
    assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
    let handle = app.active.as_ref().unwrap().handle.clone();
    // A terminal failure exits run_loop before its normal Finished branch.
    tokio::time::timeout(Duration::from_secs(5), app.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(app.active.is_none());
    // SAFETY: cleanup must have reaped the previously owned fixture process.
    let process_state = unsafe { libc::kill(pid, 0) };
    let process_error = std::io::Error::last_os_error().raw_os_error();
    let settled = handle.background_events().unwrap().iter().any(|event| {
        matches!(
            event.kind,
            saya_core::background::BackgroundEventKind::Finished { .. }
        )
    });
    // Keep the fixture contained when testing the pre-fix teardown.
    handle.shutdown_background().await.unwrap();
    assert_eq!(process_state, -1);
    assert_eq!(process_error, Some(libc::ESRCH));
    assert!(settled);
}

#[test]
fn refreshed_credentials_are_redacted_from_notices_and_terminal_titles() {
    let (_root, mut app) = fixture();
    assert_eq!(
        app.safe("synthetic-refresh-token"),
        "synthetic-refresh-token"
    );
    app.secret_registry
        .register(&["synthetic-refresh-token".into()])
        .unwrap();
    assert_eq!(
        app.safe("prefix synthetic-refresh-token suffix"),
        "prefix [REDACTED] suffix"
    );
    assert!(
        !app.safe_title("synthetic-refresh-token")
            .contains("synthetic-refresh-token")
    );
    app.notice("Failed with synthetic-refresh-token");
    assert!(
        !app.state
            .messages
            .back()
            .unwrap()
            .text
            .contains("synthetic-refresh-token")
    );
}
