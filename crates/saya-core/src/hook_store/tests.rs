use super::*;
use crate::{
    contract::{PermissionMode, Run, RunId, SessionId, WorkMode},
    hooks::{
        AnnotationLevel, CleanupState, HookAnnotation, HookEvent, HookExecutionState, HookKind,
        HookStatus,
    },
};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
};

fn run() -> Run {
    Run {
        id: RunId::new(),
        session_id: SessionId::new(),
        work_mode: WorkMode::Work,
        permissions: PermissionMode::Auto,
    }
}
fn intent(run: &Run, number: usize) -> HookIntent {
    HookIntent {
        invocation_id: validation::sha256(format!("invocation-{number}").as_bytes()),
        fingerprint: validation::sha256(format!("fingerprint-{number}").as_bytes()),
        event_id: format!("event-{number}"),
        root_event_id: format!("event-{number}"),
        event: HookEvent::RunStarted,
        kind: HookKind::Observer,
        run: run.clone(),
        parent_call_id: None,
        depth: 0,
        plugin_id: "io.saya.fixture".into(),
        package_digest: "a".repeat(64),
        entry_id: "entry".into(),
        entry_sha256: "b".repeat(64),
        lease_id: "fixture-lease".into(),
        enable_revision: 1,
        payload_sha256: "c".repeat(64),
        timeout_ms: 1000,
        output_max_bytes: 16 * 1024,
        run_invocation_limit: 256,
    }
}
fn record(intent: &HookIntent) -> HookRecord {
    HookRecord {
        intent: intent.clone(),
        status: HookStatus::Succeeded,
        execution: HookExecutionState::Started,
        cleanup: CleanupState::Cleaned,
        decision: None,
        annotations: Vec::new(),
        error: None,
    }
}
fn open(root: &Path) -> HookStore {
    HookStore::open(
        root,
        Arc::new(SecretRegistry::default()),
        HookStoreLimits::default(),
    )
    .unwrap()
}

#[tokio::test]
async fn sqlite_claim_result_and_idempotent_finish_survive_reopening() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("hooks");
    let store = open(&root);
    let intent = intent(&run(), 1);
    assert!(matches!(
        store.claim(&intent).await.unwrap(),
        HookClaim::Execute
    ));
    assert!(matches!(
        store.claim(&intent).await.unwrap(),
        HookClaim::Running
    ));
    let result = record(&intent);
    store.finish(&result).await.unwrap();
    store.finish(&result).await.unwrap();
    let HookClaim::Completed(saved) = store.claim(&intent).await.unwrap() else {
        panic!("completed result");
    };
    assert_eq!(*saved, result);
    let mut changed = result.clone();
    changed.annotations.push(HookAnnotation {
        level: AnnotationLevel::Info,
        message: "different result".into(),
    });
    assert_eq!(
        store.finish(&changed).await.unwrap_err().0.code,
        "hook_journal_conflict"
    );
    drop(store);
    let reopened = open(&root);
    assert_eq!(
        reopened
            .inspect(&intent.invocation_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        HookJournalState::Completed
    );
    assert!(
        matches!(reopened.claim(&intent).await.unwrap(), HookClaim::Completed(saved) if *saved == result)
    );
    reopened.finish(&result).await.unwrap();
    assert_eq!(
        fs::metadata(root.join("hooks.sqlite3")).unwrap().mode() & 0o777,
        0o600
    );
    assert_eq!(fs::metadata(&root).unwrap().mode() & 0o777, 0o700);
}

#[tokio::test]
async fn concurrent_claims_have_one_owner_and_one_atomic_run_budget() {
    let temp = tempfile::tempdir().unwrap();
    let store = open(&temp.path().join("hooks"));
    let mut first = intent(&run(), 1);
    first.run_invocation_limit = 4;
    let mut tasks = Vec::new();
    for _ in 0..24 {
        let store = store.clone();
        let intent = first.clone();
        tasks.push(tokio::spawn(
            async move { store.claim(&intent).await.unwrap() },
        ));
    }
    let mut started = 0;
    for task in tasks {
        match task.await.unwrap() {
            HookClaim::Execute => started += 1,
            HookClaim::Running => {}
            _ => panic!("single invocation result"),
        }
    }
    assert_eq!(started, 1);
    let mut tasks = Vec::new();
    for number in 2..26 {
        let store = store.clone();
        let mut intent = intent(&first.run, number);
        intent.run_invocation_limit = 4;
        tasks.push(tokio::spawn(
            async move { store.claim(&intent).await.unwrap() },
        ));
    }
    for task in tasks {
        match task.await.unwrap() {
            HookClaim::Execute => started += 1,
            HookClaim::BudgetExhausted => {}
            _ => panic!("bounded run result"),
        }
    }
    assert_eq!(started, 4);
    assert_eq!(
        store
            .inner
            .connection
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM invocations", [], |row| row
                .get::<_, usize>(0))
            .unwrap(),
        4
    );
    let mut changed_limit = first.clone();
    changed_limit.invocation_id = "d".repeat(64);
    changed_limit.run_invocation_limit = 5;
    assert!(matches!(
        store.claim(&changed_limit).await.unwrap(),
        HookClaim::Conflict
    ));
}

#[tokio::test]
async fn exclusive_owner_lock_fences_live_instances_and_restart_marks_only_unfinished_unknown() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("hooks");
    let store = open(&root);
    let clone = store.clone();
    let run = run();
    let pending = intent(&run, 1);
    let completed = intent(&run, 2);
    store.claim(&pending).await.unwrap();
    store.claim(&completed).await.unwrap();
    store.finish(&record(&completed)).await.unwrap();
    assert_eq!(
        HookStore::open(
            &root,
            Arc::new(SecretRegistry::default()),
            HookStoreLimits::default()
        )
        .unwrap_err()
        .0
        .code,
        "hook_journal_busy"
    );
    drop(store);
    assert_eq!(
        HookStore::open(
            &root,
            Arc::new(SecretRegistry::default()),
            HookStoreLimits::default()
        )
        .unwrap_err()
        .0
        .code,
        "hook_journal_busy"
    );
    drop(clone);
    let recovered = open(&root);
    assert!(matches!(
        recovered.claim(&pending).await.unwrap(),
        HookClaim::Unknown
    ));
    assert_eq!(
        recovered
            .inspect(&pending.invocation_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        HookJournalState::Unknown
    );
    assert!(matches!(
        recovered.claim(&completed).await.unwrap(),
        HookClaim::Completed(_)
    ));
    assert_eq!(
        recovered
            .finish(&record(&pending))
            .await
            .unwrap_err()
            .0
            .code,
        "hook_journal_unknown"
    );
    let mut another = intent(&run, 3);
    another.run_invocation_limit = 2;
    assert!(matches!(
        recovered.claim(&another).await.unwrap(),
        HookClaim::Conflict
    ));
}

#[test]
fn crash_process_helper() {
    let Some(root) = std::env::var_os("SAYA_TEST_HOOK_CRASH_DIRECTORY") else {
        return;
    };
    let intent: HookIntent =
        serde_json::from_str(&std::env::var("SAYA_TEST_HOOK_CRASH_INTENT").unwrap()).unwrap();
    let store = open(Path::new(&root));
    store
        .inner
        .fail_after_claim_commit
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    assert_eq!(
        runtime.block_on(store.claim(&intent)).err().unwrap().0.code,
        "hook_journal_unknown"
    );
    // Bypass all Rust destructors to exercise the actual SQLite WAL/OS lease
    // crash boundary, after the intent's durable transaction completed.
    std::process::exit(29);
}

#[tokio::test]
async fn process_exit_after_durable_intent_recovers_unknown_without_replay() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("hooks");
    let intent = intent(&run(), 1);
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "hook_store::tests::crash_process_helper",
            "--nocapture",
        ])
        .env("SAYA_TEST_HOOK_CRASH_DIRECTORY", &root)
        .env(
            "SAYA_TEST_HOOK_CRASH_INTENT",
            serde_json::to_string(&intent).unwrap(),
        )
        .output()
        .unwrap();
    assert_eq!(
        status.status.code(),
        Some(29),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let recovered = open(&root);
    assert!(matches!(
        recovered.claim(&intent).await.unwrap(),
        HookClaim::Unknown
    ));
    assert!(
        recovered
            .inspect(&intent.invocation_id)
            .await
            .unwrap()
            .unwrap()
            .record
            .is_none()
    );
}

#[tokio::test]
async fn identity_and_result_conflicts_and_impossible_states_do_not_replace_durable_records() {
    let temp = tempfile::tempdir().unwrap();
    let store = open(&temp.path().join("hooks"));
    let intent = intent(&run(), 1);
    store.claim(&intent).await.unwrap();
    let mut changed = intent.clone();
    changed.payload_sha256 = "d".repeat(64);
    assert!(matches!(
        store.claim(&changed).await.unwrap(),
        HookClaim::Conflict
    ));
    assert!(store.finish(&record(&changed)).await.is_err());
    let mut invalid = record(&intent);
    invalid.cleanup = CleanupState::Unknown;
    assert_eq!(
        store.finish(&invalid).await.unwrap_err().0.code,
        "hook_journal_input"
    );
    invalid = record(&intent);
    invalid.status = HookStatus::Unknown;
    invalid.cleanup = CleanupState::Unknown;
    store.finish(&invalid).await.unwrap();
    store.finish(&invalid).await.unwrap();
    assert!(matches!(
        store.claim(&intent).await.unwrap(),
        HookClaim::Unknown
    ));
    assert_eq!(
        store
            .inspect(&intent.invocation_id)
            .await
            .unwrap()
            .unwrap()
            .record
            .unwrap(),
        invalid
    );
    assert!(store.finish(&record(&intent)).await.is_err());
}

#[tokio::test]
async fn diagnostics_are_deduplicated_and_capacity_is_checked_before_new_intents() {
    let temp = tempfile::tempdir().unwrap();
    let limits = HookStoreLimits {
        max_invocations: 1,
        max_diagnostics: 1,
        ..HookStoreLimits::default()
    };
    let store = HookStore::open(
        &temp.path().join("hooks"),
        Arc::new(SecretRegistry::default()),
        limits,
    )
    .unwrap();
    let first = intent(&run(), 1);
    store.claim(&first).await.unwrap();
    let second = intent(&run(), 2);
    assert_eq!(
        store.claim(&second).await.err().unwrap().0.code,
        "hook_journal_full"
    );
    let note = HookDiagnostic {
        invocation_id: first.invocation_id.clone(),
        event_id: first.event_id.clone(),
        code: "hook_recursion".into(),
        message: "Recursion suppressed".into(),
    };
    store.diagnostic(&note).await.unwrap();
    store.diagnostic(&note).await.unwrap();
    let mut changed = note;
    changed.message = "Another diagnostic".into();
    assert_eq!(
        store.diagnostic(&changed).await.unwrap_err().0.code,
        "hook_journal_full"
    );
    let connection = store.inner.connection.lock().unwrap();
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM diagnostics", [], |row| row
                .get::<_, usize>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM runs", [], |row| row
                .get::<_, usize>(0))
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn known_and_rotated_credentials_are_rejected_or_redacted_before_sqlite_persistence() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("hooks");
    let secrets = Arc::new(SecretRegistry::default());
    secrets.register(&["fixture-known-secret".into()]).unwrap();
    let store = HookStore::open(&root, secrets.clone(), HookStoreLimits::default()).unwrap();
    let mut intent = intent(&run(), 1);
    intent.event_id = "fixture-known-secret".into();
    intent.root_event_id = intent.event_id.clone();
    assert_eq!(
        store.claim(&intent).await.err().unwrap().0.code,
        "hook_journal_secret"
    );
    intent.event_id = "event-1".into();
    intent.root_event_id = "event-1".into();
    store.claim(&intent).await.unwrap();
    secrets
        .register(&["fixture-rotated-secret".into()])
        .unwrap();
    let mut result = record(&intent);
    result.annotations.push(HookAnnotation {
        level: AnnotationLevel::Info,
        message: "credential=fixture-rotated-secret".into(),
    });
    store.finish(&result).await.unwrap();
    store.finish(&result).await.unwrap();
    let note = HookDiagnostic {
        invocation_id: intent.invocation_id.clone(),
        event_id: intent.event_id.clone(),
        code: "hook_note".into(),
        message: "credential=fixture-known-secret".into(),
    };
    store.diagnostic(&note).await.unwrap();
    let saved = store.inspect(&intent.invocation_id).await.unwrap().unwrap();
    assert!(
        !serde_json::to_string(&saved.record)
            .unwrap()
            .contains("fixture-rotated-secret")
    );
    drop(store);
    for entry in fs::read_dir(&root).unwrap() {
        let bytes = fs::read(entry.unwrap().path()).unwrap();
        for secret in [
            b"fixture-known-secret".as_slice(),
            b"fixture-rotated-secret".as_slice(),
        ] {
            assert!(!bytes.windows(secret.len()).any(|window| window == secret));
        }
    }
}

#[tokio::test]
async fn sqlite_write_failures_leave_no_claim_or_keep_the_original_running_intent() {
    let temp = tempfile::tempdir().unwrap();
    let store = open(&temp.path().join("hooks"));
    let intent = intent(&run(), 1);
    store
        .inner
        .connection
        .lock()
        .unwrap()
        .pragma_update(None, "query_only", true)
        .unwrap();
    assert!(store.claim(&intent).await.is_err());
    store
        .inner
        .connection
        .lock()
        .unwrap()
        .pragma_update(None, "query_only", false)
        .unwrap();
    assert!(
        store
            .inspect(&intent.invocation_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        store.claim(&intent).await.unwrap(),
        HookClaim::Execute
    ));
    store
        .inner
        .connection
        .lock()
        .unwrap()
        .pragma_update(None, "query_only", true)
        .unwrap();
    assert!(store.finish(&record(&intent)).await.is_err());
    store
        .inner
        .connection
        .lock()
        .unwrap()
        .pragma_update(None, "query_only", false)
        .unwrap();
    assert!(matches!(
        store.claim(&intent).await.unwrap(),
        HookClaim::Running
    ));
    store.finish(&record(&intent)).await.unwrap();
}

#[tokio::test]
async fn malformed_controls_oversized_annotations_and_scope_changes_fail_before_writes() {
    let temp = tempfile::tempdir().unwrap();
    let store = open(&temp.path().join("hooks"));
    let base = intent(&run(), 1);
    let mut invalid = base.clone();
    invalid.event_id = "bad\u{1b}event".into();
    assert!(store.claim(&invalid).await.is_err());
    invalid = base.clone();
    invalid.invocation_id = "not-a-digest".into();
    assert!(store.claim(&invalid).await.is_err());
    invalid = base.clone();
    invalid.kind = HookKind::Check;
    assert!(store.claim(&invalid).await.is_err());
    invalid = base.clone();
    invalid.parent_call_id = Some("invalid-parent".into());
    assert!(store.claim(&invalid).await.is_err());
    let mut invalid = base.clone();
    invalid.run_invocation_limit = 4097;
    assert!(store.claim(&invalid).await.is_err());
    store.claim(&base).await.unwrap();
    let mut changed = intent(&base.run, 2);
    changed.run.session_id = SessionId::new();
    assert!(matches!(
        store.claim(&changed).await.unwrap(),
        HookClaim::Conflict
    ));
    let mut result = record(&base);
    result.annotations.push(HookAnnotation {
        level: AnnotationLevel::Info,
        message: "x".repeat(1025),
    });
    assert!(store.finish(&result).await.is_err());
    assert!(matches!(
        store.claim(&base).await.unwrap(),
        HookClaim::Running
    ));
}

#[test]
fn unsafe_directories_database_links_and_sidecars_are_rejected_without_following() {
    let temp = tempfile::tempdir().unwrap();
    let outside = temp.path().join("outside");
    fs::write(&outside, "external bytes").unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).unwrap();
    for name in [
        ".owner.lock",
        "hooks.sqlite3",
        "hooks.sqlite3-wal",
        "hooks.sqlite3-shm",
    ] {
        let root = temp.path().join(name.replace('.', "-"));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        symlink(&outside, root.join(name)).unwrap();
        assert!(
            HookStore::open(
                &root,
                Arc::new(SecretRegistry::default()),
                HookStoreLimits::default()
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(&outside).unwrap(), "external bytes");
    }
    let hardlinked = temp.path().join("hardlinked");
    fs::create_dir(&hardlinked).unwrap();
    fs::set_permissions(&hardlinked, fs::Permissions::from_mode(0o700)).unwrap();
    fs::hard_link(&outside, hardlinked.join("hooks.sqlite3")).unwrap();
    assert!(
        HookStore::open(
            &hardlinked,
            Arc::new(SecretRegistry::default()),
            HookStoreLimits::default()
        )
        .is_err()
    );
    let public = temp.path().join("public");
    fs::create_dir(&public).unwrap();
    fs::set_permissions(&public, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        HookStore::open(
            &public,
            Arc::new(SecretRegistry::default()),
            HookStoreLimits::default()
        )
        .is_err()
    );
    let linked = temp.path().join("linked-root");
    symlink(&public, &linked).unwrap();
    assert!(
        HookStore::open(
            &linked,
            Arc::new(SecretRegistry::default()),
            HookStoreLimits::default()
        )
        .is_err()
    );
}

#[tokio::test]
async fn root_and_database_replacement_and_incompatible_schema_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("hooks");
    let store = open(&root);
    fs::rename(&root, temp.path().join("old-hooks")).unwrap();
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        store.claim(&intent(&run(), 1)).await.err().unwrap().0.code,
        "hook_journal_permissions"
    );
    drop(store);
    let root = temp.path().join("second");
    let store = open(&root);
    store
        .inner
        .connection
        .lock()
        .unwrap()
        .execute_batch("CREATE TABLE unexpected (data TEXT)")
        .unwrap();
    drop(store);
    assert_eq!(
        HookStore::open(
            &root,
            Arc::new(SecretRegistry::default()),
            HookStoreLimits::default()
        )
        .unwrap_err()
        .0
        .code,
        "hook_journal_corrupt"
    );
    let root = temp.path().join("third");
    let store = open(&root);
    fs::rename(root.join("hooks.sqlite3"), root.join("old-database")).unwrap();
    fs::write(root.join("hooks.sqlite3"), "replacement").unwrap();
    assert!(store.claim(&intent(&run(), 1)).await.is_err());
}
