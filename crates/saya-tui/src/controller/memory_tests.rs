use super::*;
use saya_core::global_memory::{
    GlobalMemoryStore, MemoryChange, MemoryEntry, MemoryFact, MemoryKind, MemoryLimits,
    MemoryScope, MemorySource, MemorySourceKind, MemorySourceRef, WorkspaceIdentity,
};
use std::time::Duration;

fn seed(app: &App) -> GlobalMemoryStore {
    let store = GlobalMemoryStore::open(
        &app.config.memory_root().unwrap(),
        WorkspaceIdentity::from_path(&app.config.cwd).unwrap(),
        MemoryLimits::default(),
        app.secret_registry.clone(),
    )
    .unwrap();
    let source = MemorySource {
        id: "src_user".into(),
        kind: MemorySourceKind::User,
        scope: MemoryScope::Global,
        reference: "explicit user".into(),
        revision: "a".repeat(64),
        observed_at: "2026-11-10".into(),
        description: "User preference".into(),
        metadata_withheld: false,
    };
    let entry = MemoryEntry {
        id: "mem_test".into(),
        title: "Development preference".into(),
        scope: MemoryScope::Global,
        updated_at: "2026-11-10".into(),
        facts: vec![MemoryFact {
            id: "fact_test".into(),
            kind: MemoryKind::ExplicitPreference,
            text: "Prefer focused changes".into(),
            sources: vec![MemorySourceRef {
                source_id: source.id.clone(),
                revision: source.revision.clone(),
            }],
            aliases: vec![],
            derived_from: vec![],
        }],
        resources: vec![],
    };
    let proposal = store
        .propose(
            &store.snapshot().unwrap().revision,
            "op_seed",
            MemoryChange::Upsert {
                entry,
                sources: vec![source],
            },
        )
        .unwrap();
    store.commit(&proposal).unwrap();
    store
}
async fn complete(app: &mut App) {
    let result = tokio::time::timeout(Duration::from_secs(8), services::next_job(&mut app.ui_job))
        .await
        .unwrap();
    app.finish_ui(result).await.unwrap();
}
async fn type_command(app: &mut App, text: &str) {
    app.state
        .memory_panel
        .as_mut()
        .unwrap()
        .input
        .set_text(text);
    app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
}

#[tokio::test]
async fn memory_modal_keeps_chat_draft_and_reads_current_scoped_sources_without_a_run() {
    let (_root, mut app) = super::control_tests::fixture();
    seed(&app);
    app.state.editor.set_text("Retain this unsent draft");
    app.command("/memory").await.unwrap();
    complete(&mut app).await;
    assert!(
        app.state
            .memory_panel
            .as_ref()
            .unwrap()
            .lines
            .join("\n")
            .contains("mem_test")
    );
    assert!(app.active.is_none() && app.prepared.is_none());
    type_command(&mut app, "show fact_test").await;
    complete(&mut app).await;
    let body = app.state.memory_panel.as_ref().unwrap().lines.join("\n");
    assert!(body.contains("Prefer focused changes") && body.contains("snapshot_revision"));
    type_command(&mut app, "sources fact_test").await;
    complete(&mut app).await;
    assert!(
        app.state
            .memory_panel
            .as_ref()
            .unwrap()
            .lines
            .join("\n")
            .contains("explicit user")
    );
    app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.state.memory_panel.is_none());
    assert_eq!(app.state.editor.text(), "Retain this unsent draft");
}
#[tokio::test]
async fn correction_and_forgetting_require_reviewed_exact_confirmation_and_leave_receipts() {
    let (_root, mut app) = super::control_tests::fixture();
    let store = seed(&app);
    app.command("/memory").await.unwrap();
    complete(&mut app).await;
    let original = store.snapshot().unwrap().revision;
    type_command(&mut app, "correct fact_test Prefer bounded reviews").await;
    complete(&mut app).await;
    let review = app.state.memory_panel.as_ref().unwrap().lines.join("\n");
    assert!(review.contains("Prefer focused changes") && review.contains("Prefer bounded reviews"));
    assert_eq!(store.snapshot().unwrap().revision, original);
    type_command(&mut app, "confirm wrong wrong").await;
    assert!(app.ui_job.is_none() && app.memory_pending.is_some());
    let confirmation = app
        .state
        .memory_panel
        .as_ref()
        .unwrap()
        .confirmation
        .clone()
        .unwrap();
    let operation_id = app
        .memory_pending
        .as_ref()
        .unwrap()
        .operation_id()
        .to_owned();
    type_command(&mut app, &confirmation).await;
    complete(&mut app).await;
    assert_eq!(
        store.snapshot().unwrap().entries[0].facts[0].text,
        "Prefer bounded reviews"
    );
    assert!(app.memory_pending.is_none());
    assert_eq!(
        app.memory_last_operation.as_deref(),
        Some(operation_id.as_str())
    );
    assert!(
        app.state
            .memory_panel
            .as_ref()
            .unwrap()
            .lines
            .join("\n")
            .contains("receipt")
    );
    type_command(&mut app, "forget fact_test").await;
    complete(&mut app).await;
    let confirmation = app
        .state
        .memory_panel
        .as_ref()
        .unwrap()
        .confirmation
        .clone()
        .unwrap();
    type_command(&mut app, &confirmation).await;
    complete(&mut app).await;
    assert!(store.snapshot().unwrap().entries.is_empty());
    assert!(
        !app.state
            .memory_panel
            .as_ref()
            .unwrap()
            .lines
            .join("\n")
            .contains("Prefer bounded reviews")
    );
    assert!(app.memory_pending.is_none() && app.active.is_none());
}
#[tokio::test]
async fn stale_review_plan_and_write_disable_fail_without_losing_draft_or_replaying() {
    let (_root, mut app) = super::control_tests::fixture();
    let store = seed(&app);
    app.state.editor.set_text("Message draft");
    app.command("/memory").await.unwrap();
    complete(&mut app).await;
    type_command(&mut app, "forget fact_test").await;
    complete(&mut app).await;
    let confirmation = app
        .state
        .memory_panel
        .as_ref()
        .unwrap()
        .confirmation
        .clone()
        .unwrap();
    let admin = saya_app::memory_admin::MemoryAdmin::open(&app.config, app.secret_registry.clone())
        .unwrap();
    let stopping = std::sync::atomic::AtomicBool::new(false);
    let change = admin
        .prepare("correct fact_test Fresh external correction", &stopping)
        .unwrap();
    admin.confirm(change, &stopping).unwrap();
    type_command(&mut app, &confirmation).await;
    complete(&mut app).await;
    assert!(
        app.state
            .memory_panel
            .as_ref()
            .unwrap()
            .lines
            .join("\n")
            .contains("memory_conflict")
    );
    assert!(app.memory_pending.is_none());
    assert_eq!(
        store.snapshot().unwrap().entries[0].facts[0].text,
        "Fresh external correction"
    );
    app.config.mode = WorkMode::Plan;
    type_command(&mut app, "forget fact_test").await;
    complete(&mut app).await;
    assert!(
        app.state
            .memory_panel
            .as_ref()
            .unwrap()
            .lines
            .join("\n")
            .contains("mode_denied")
    );
    app.config.mode = WorkMode::Work;
    app.config.memory.write = false;
    type_command(&mut app, "forget fact_test").await;
    complete(&mut app).await;
    assert!(
        app.state
            .memory_panel
            .as_ref()
            .unwrap()
            .lines
            .join("\n")
            .contains("memory_write_disabled")
    );
    assert_eq!(app.state.editor.text(), "Message draft");
}
#[tokio::test]
async fn process_settings_reject_clear_and_cancellation_keep_chat_input_and_uncommitted_state() {
    let (_root, mut app) = super::control_tests::fixture();
    let store = seed(&app);
    app.state.editor.set_text("Keep message");
    app.command("/memory").await.unwrap();
    complete(&mut app).await;
    type_command(&mut app, "forget fact_test").await;
    complete(&mut app).await;
    type_command(&mut app, "reject").await;
    assert!(app.memory_pending.is_none());
    assert!(!store.snapshot().unwrap().entries.is_empty());
    type_command(&mut app, "set read off").await;
    assert!(!app.config.memory.read);
    assert!(
        app.state
            .memory_panel
            .as_ref()
            .unwrap()
            .settings
            .contains("read false")
    );
    type_command(&mut app, "forget fact_test").await;
    complete(&mut app).await;
    assert!(app.memory_pending.is_none());
    assert!(
        app.state
            .memory_panel
            .as_ref()
            .unwrap()
            .lines
            .join("\n")
            .contains("enable read")
    );
    type_command(&mut app, "set read on").await;
    type_command(&mut app, "set inject off").await;
    type_command(&mut app, "set write off").await;
    assert!(app.config.memory.read && !app.config.memory.inject && !app.config.memory.write);
    app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.state.editor.text(), "Keep message");
}
#[tokio::test]
async fn modal_search_continuation_and_rotated_secret_view_are_bounded() {
    let (_root, mut app) = super::control_tests::fixture();
    seed(&app);
    app.command("/memory search focused").await.unwrap();
    complete(&mut app).await;
    assert!(
        app.state
            .memory_panel
            .as_ref()
            .unwrap()
            .lines
            .join("\n")
            .contains("focused")
    );
    app.secret_registry
        .register(&["Prefer focused changes".into()])
        .unwrap();
    assert!(app.scrub_memory());
    assert!(
        !app.state
            .memory_panel
            .as_ref()
            .unwrap()
            .lines
            .join("\n")
            .contains("Prefer focused changes")
    );
    app.memory_command("help").await.unwrap();
    assert!(
        app.state
            .memory_panel
            .as_ref()
            .unwrap()
            .lines
            .iter()
            .any(|line| line.contains("current process"))
    );
}

#[tokio::test]
async fn disabling_read_invalidates_an_existing_full_review() {
    let (_root, mut app) = super::control_tests::fixture();
    let store = seed(&app);
    app.command("/memory").await.unwrap();
    complete(&mut app).await;
    type_command(&mut app, "forget fact_test").await;
    complete(&mut app).await;
    let confirmation = app
        .state
        .memory_panel
        .as_ref()
        .unwrap()
        .confirmation
        .clone()
        .unwrap();
    type_command(&mut app, "set read off").await;
    assert!(app.memory_pending.is_none());
    assert!(
        !app.state
            .memory_panel
            .as_ref()
            .unwrap()
            .lines
            .join("\n")
            .contains("Prefer focused changes")
    );
    type_command(&mut app, &confirmation).await;
    assert!(app.ui_job.is_none());
    assert!(!store.snapshot().unwrap().entries.is_empty());
}

#[tokio::test]
async fn active_run_refuses_memory_mutations_and_process_settings_but_allows_queries() {
    let (_root, mut app) = super::control_tests::fixture();
    let store = seed(&app);
    super::control_tests::attach_active_call(&mut app).await;
    app.command("/memory").await.unwrap();
    complete(&mut app).await;
    assert!(app.memory_command("forget fact_test").await.is_err());
    assert!(app.memory_command("set write off").await.is_err());
    assert!(app.memory_pending.is_none() && app.ui_job.is_none());
    assert!(!store.snapshot().unwrap().entries.is_empty());
    app.memory_command("show fact_test").await.unwrap();
    complete(&mut app).await;
    assert!(
        app.state
            .memory_panel
            .as_ref()
            .unwrap()
            .lines
            .join("\n")
            .contains("focused")
    );
    app.memory_command("close").await.unwrap();
    super::control_tests::stop_active_call(&mut app).await;
}
