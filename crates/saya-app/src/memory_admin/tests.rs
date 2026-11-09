use super::*;
use saya_core::{
    config::{Environment, Overrides},
    global_memory::{MemoryEntry, MemoryFact, MemoryScope},
};
use std::ffi::OsString;

pub(super) fn fixture() -> (tempfile::TempDir, MemoryAdmin) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    std::fs::write(&path, "schema_version = 1\n").unwrap();
    let config = saya_core::config::load(
        Overrides {
            config: Some(path),
            ..Default::default()
        },
        Environment::from_pairs([(OsString::from("HOME"), root.path().as_os_str().to_owned())]),
        root.path(),
    )
    .unwrap();
    let admin = MemoryAdmin::open(&config, Arc::new(SecretRegistry::default())).unwrap();
    (root, admin)
}
pub(super) fn seed(admin: &MemoryAdmin) -> String {
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
            aliases: vec!["focused".into()],
            derived_from: vec![],
        }],
        resources: vec![],
    };
    let proposal = admin
        .store
        .propose(
            &admin.store.snapshot().unwrap().revision,
            "op_seed",
            MemoryChange::Upsert {
                entry,
                sources: vec![source],
            },
        )
        .unwrap();
    admin.store.commit(&proposal).unwrap();
    admin.store.snapshot().unwrap().revision
}
pub(super) fn cancel() -> AtomicBool {
    AtomicBool::new(false)
}

#[test]
fn missing_store_queries_stay_read_only_and_disabled_read_is_enforced() {
    let (_root, mut admin) = fixture();
    assert_eq!(admin.query("list", &cancel()).unwrap()["data"]["total"], 0);
    assert!(!admin.store.root().exists());
    admin.config.memory.read = false;
    assert_eq!(
        admin.query("list", &cancel()).unwrap_err().0.code,
        "memory_read_disabled"
    );
    assert_eq!(admin.settings().unwrap()["read"], false);
}
#[test]
fn correction_review_is_pure_complete_and_confirm_binds_original_revision() {
    let (_root, admin) = fixture();
    let original = seed(&admin);
    let prepared = admin
        .prepare("correct fact_test Prefer bounded changes", &cancel())
        .unwrap();
    assert_eq!(prepared.expected_revision(), original);
    assert!(prepared.operation_id().starts_with("op_"));
    assert_eq!(
        prepared.review()["before"][0]["facts"][0]["text"],
        "Prefer focused changes"
    );
    assert_eq!(
        prepared.review()["after"][0]["facts"][0]["text"],
        "Prefer bounded changes"
    );
    assert_eq!(admin.store.snapshot().unwrap().revision, original);
    let newer = admin
        .prepare("correct fact_test Prefer reviewed changes", &cancel())
        .unwrap();
    admin.confirm(newer, &cancel()).unwrap();
    assert_eq!(
        admin.confirm(prepared, &cancel()).unwrap_err().0.code,
        "memory_conflict"
    );
    assert_eq!(
        admin.query("show fact_test", &cancel()).unwrap()["data"]["items"][0]["text"],
        "Prefer reviewed changes"
    );
}
#[test]
fn confirmation_rechecks_plan_write_scope_and_rotated_credentials() {
    let (_root, mut admin) = fixture();
    seed(&admin);
    let prepared = admin.prepare("forget fact_test", &cancel()).unwrap();
    admin.config.mode = WorkMode::Plan;
    assert_eq!(
        admin.confirm(prepared, &cancel()).unwrap_err().0.code,
        "mode_denied"
    );
    admin.config.mode = WorkMode::Work;
    let prepared = admin.prepare("forget fact_test", &cancel()).unwrap();
    admin.config.memory.write = false;
    assert_eq!(
        admin.confirm(prepared, &cancel()).unwrap_err().0.code,
        "memory_write_disabled"
    );
    admin.config.memory.write = true;
    let prepared = admin
        .prepare("correct fact_test rotation-test-protected-value", &cancel())
        .unwrap();
    admin
        .secrets
        .register(&["rotation-test-protected-value".into()])
        .unwrap();
    assert_eq!(
        admin.confirm(prepared, &cancel()).unwrap_err().0.code,
        "sensitive_data"
    );
    let other = tempfile::tempdir().unwrap();
    let mut config = admin.config.clone();
    config.cwd = other.path().to_path_buf();
    let other_admin = MemoryAdmin::open(&config, admin.secrets.clone()).unwrap();
    let prepared = admin.prepare("forget fact_test", &cancel()).unwrap();
    assert_eq!(
        other_admin.confirm(prepared, &cancel()).unwrap_err().0.code,
        "memory_scope"
    );
}
#[test]
fn forgotten_facts_do_not_return_through_history_rollback_or_backups() {
    let (_root, admin) = fixture();
    seed(&admin);
    let prepared = admin
        .prepare("correct fact_test Prefer reviewed changes", &cancel())
        .unwrap();
    admin.confirm(prepared, &cancel()).unwrap();
    let prepared = admin.prepare("forget fact_test", &cancel()).unwrap();
    let result = admin.confirm(prepared, &cancel()).unwrap();
    assert_eq!(result["status"], "committed");
    assert!(result["historical_copies_retained"].as_bool().unwrap());
    for version in admin.store.history().unwrap() {
        let prepared = admin
            .prepare(&format!("rollback {}", version.version), &cancel())
            .unwrap();
        admin.confirm(prepared, &cancel()).unwrap();
    }
    assert!(admin.store.snapshot().unwrap().entries.is_empty());
    let backup = admin.store.export_backup().unwrap();
    assert!(backup.document.entries.is_empty());
    assert!(!backup.coverage.topic_bodies && !backup.coverage.historical_versions);
}
#[test]
fn export_uses_reviewed_bytes_and_restore_uses_captured_input() {
    let (root, admin) = fixture();
    seed(&admin);
    let target = root.path().join("backup.json");
    let prepared = admin
        .prepare(&format!("export {}", target.display()), &cancel())
        .unwrap();
    assert!(!target.exists());
    assert_eq!(prepared.review()["coverage"]["topic_bodies"], false);
    admin.confirm(prepared, &cancel()).unwrap();
    let prepared = admin
        .prepare(&format!("restore {}", target.display()), &cancel())
        .unwrap();
    std::fs::write(&target, "invalid after review").unwrap();
    admin.confirm(prepared, &cancel()).unwrap();
    assert_eq!(
        admin.store.snapshot().unwrap().entries[0].facts[0].text,
        "Prefer focused changes"
    );
    let symlink = root.path().join("linked.json");
    std::os::unix::fs::symlink(&target, &symlink).unwrap();
    assert_eq!(
        admin
            .prepare(&format!("restore {}", symlink.display()), &cancel())
            .err()
            .unwrap()
            .0
            .code,
        "memory_input_io"
    );
}
#[test]
fn cancellation_and_oversized_review_never_publish() {
    let (_root, admin) = fixture();
    let revision = seed(&admin);
    let prepared = admin.prepare("forget fact_test", &cancel()).unwrap();
    let stopped = AtomicBool::new(true);
    assert_eq!(
        admin.confirm(prepared, &stopped).unwrap_err().0.code,
        "cancelled"
    );
    assert_eq!(admin.store.snapshot().unwrap().revision, revision);
    assert!(
        admin
            .prepare(
                &format!("correct fact_test {}", "x".repeat(MAX_VIEW_BYTES)),
                &cancel()
            )
            .is_err()
    );
    assert_eq!(admin.store.snapshot().unwrap().revision, revision);
}
