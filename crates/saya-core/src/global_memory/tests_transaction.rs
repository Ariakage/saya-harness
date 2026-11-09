use super::tests::{code, document, fact, save, source, store};
use super::*;
use std::{
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    sync::atomic::AtomicBool,
};
use tempfile::TempDir;
use transaction::{FaultPoint, fail_once};

fn update(store: &GlobalMemoryStore, operation_id: &str, text: &str) -> MemoryProposal {
    let mut document = document();
    document.entries[0].facts[0].text = text.into();
    store
        .propose(
            &store.snapshot().unwrap().revision,
            operation_id,
            MemoryChange::Upsert {
                entry: document.entries.remove(0),
                sources: document.sources,
            },
        )
        .unwrap()
}
fn commit_initial(store: &GlobalMemoryStore) -> MemoryProposal {
    let proposal = update(store, "op_initial", "Prefer focused changes.");
    assert_eq!(
        store.commit(&proposal).unwrap().status,
        MemoryCommitStatus::Committed
    );
    proposal
}

#[test]
fn authorized_first_commit_is_private_and_survives_reopen() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let proposal = update(&store, "op_first", "Prefer focused changes.");
    assert!(!store.root().exists());
    let outcome = store.commit(&proposal).unwrap();
    assert_eq!(outcome.status, MemoryCommitStatus::Committed);
    assert!(outcome.error.is_none());
    assert!(!outcome.historical_copies_retained);
    assert_eq!(outcome.revision, Some(store.snapshot().unwrap().revision));
    assert_eq!(
        std::fs::metadata(store.root()).unwrap().mode() & 0o777,
        0o700
    );
    for path in ["MEMORY.md", ".saya-memory.lock", state::STATE_PATH] {
        assert_eq!(
            std::fs::metadata(store.root().join(path)).unwrap().mode() & 0o777,
            0o600
        );
    }
    let reopened = GlobalMemoryStore::open(
        store.root(),
        store.workspace().clone(),
        store.limits(),
        Arc::new(SecretRegistry::default()),
    )
    .unwrap();
    assert_eq!(
        reopened.snapshot().unwrap().entries[0].facts[0].text,
        "Prefer focused changes."
    );
    assert!(reopened.recover().unwrap().is_none());
}

#[test]
fn retries_are_idempotent_and_operation_identity_cannot_be_repurposed() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let initial = commit_initial(&store);
    let revision = store.snapshot().unwrap().revision;
    let retry = store.commit(&initial).unwrap();
    assert_eq!(retry.status, MemoryCommitStatus::AlreadyCommitted);
    assert_eq!(retry.receipt.generation, 1);
    assert_eq!(store.snapshot().unwrap().revision, revision);
    let different = update(&store, "op_initial", "A different preference.");
    assert_eq!(
        code(store.commit(&different)),
        "memory_idempotency_conflict"
    );
}

#[test]
fn external_edit_conflict_preserves_user_file_and_current_commit_state() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    commit_initial(&store);
    let proposal = update(&store, "op_reviewed", "Reviewed change.");
    let path = store.root().join("MEMORY.md");
    let edited = std::fs::read_to_string(&path)
        .unwrap()
        .replace("focused changes", "external editor changes");
    std::fs::write(&path, &edited).unwrap();
    assert_eq!(code(store.commit(&proposal)), "memory_conflict");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
    assert!(!store.root().join(state::JOURNAL_PATH).exists());
}

#[test]
fn interruption_before_wal_leaves_active_memory_and_revision_unchanged() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    commit_initial(&store);
    let revision = store.snapshot().unwrap().revision;
    let proposal = update(&store, "op_after_stage", "A staged update.");
    fail_once(FaultPoint::AfterStage);
    assert_eq!(code(store.commit(&proposal)), "memory_io");
    assert_eq!(store.snapshot().unwrap().revision, revision);
    assert!(store.recover().unwrap().is_none());
    assert_eq!(
        store.commit(&proposal).unwrap().status,
        MemoryCommitStatus::Committed
    );
}

#[test]
fn wal_index_and_ledger_interruptions_recover_once_without_replaying_change() {
    for point in [
        FaultPoint::AfterJournal,
        FaultPoint::AfterIndex,
        FaultPoint::AfterLedger,
    ] {
        let temp = TempDir::new().unwrap();
        let store = store(&temp);
        commit_initial(&store);
        let proposal = update(&store, "op_interrupted", "A recoverable update.");
        fail_once(point);
        let outcome = store.commit(&proposal).unwrap();
        assert_eq!(outcome.status, MemoryCommitStatus::Unknown);
        assert_eq!(code(store.snapshot()), "memory_recovery_required");
        let receipt = store.recover().unwrap().unwrap();
        assert_eq!(receipt.operation_id, "op_interrupted");
        assert_eq!(receipt.generation, 2);
        assert_eq!(
            store.snapshot().unwrap().entries[0].facts[0].text,
            "A recoverable update."
        );
        assert!(store.recover().unwrap().is_none());
        assert_eq!(
            store.commit(&proposal).unwrap().status,
            MemoryCommitStatus::AlreadyCommitted
        );
    }
}

#[test]
fn recovery_conflict_preserves_external_edit_and_durable_intent() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    commit_initial(&store);
    let proposal = update(&store, "op_pending", "Reviewed update.");
    fail_once(FaultPoint::AfterJournal);
    assert_eq!(
        store.commit(&proposal).unwrap().status,
        MemoryCommitStatus::Unknown
    );
    let mut external = document();
    external.entries[0].facts[0].text = "Concurrent external edit.".into();
    save(&store, &external);
    let bytes = std::fs::read(store.root().join("MEMORY.md")).unwrap();
    assert_eq!(code(store.recover()), "memory_recovery_conflict");
    assert_eq!(
        std::fs::read(store.root().join("MEMORY.md")).unwrap(),
        bytes
    );
    assert!(store.root().join(state::JOURNAL_PATH).exists());
}

#[test]
fn forget_survives_raw_old_index_replay_and_history_rollback() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut original = document();
    original.entries[0].facts.push(fact(
        "fact_independent",
        "Keep an independent theme preference.",
        &original.sources[0],
    ));
    save(&store, &original);
    let old_bytes = std::fs::read(store.root().join("MEMORY.md")).unwrap();
    let old_version = digest(&old_bytes);
    let forget = store
        .propose(
            &store.snapshot().unwrap().revision,
            "op_forget",
            MemoryChange::Forget {
                scope: MemoryScope::Global,
                fact_ids: vec!["fact_editor".into()],
            },
        )
        .unwrap();
    let outcome = store.commit(&forget).unwrap();
    assert!(outcome.historical_copies_retained);
    assert_eq!(store.snapshot().unwrap().entries[0].facts.len(), 1);
    std::fs::write(store.root().join("MEMORY.md"), &old_bytes).unwrap();
    assert_eq!(
        store.snapshot().unwrap().entries[0].facts[0].id,
        "fact_independent"
    );
    let restore = store
        .propose(
            &store.snapshot().unwrap().revision,
            "op_rollback",
            MemoryChange::RestoreVersion {
                version: old_version,
            },
        )
        .unwrap();
    assert!(
        restore
            .after_entries
            .iter()
            .flat_map(|entry| &entry.facts)
            .all(|fact| fact.id != "fact_editor")
    );
    assert_eq!(
        store.commit(&restore).unwrap().status,
        MemoryCommitStatus::Committed
    );
    assert!(
        !std::fs::read_to_string(store.root().join("MEMORY.md"))
            .unwrap()
            .contains("small focused changes")
    );
}

#[test]
fn source_withdrawal_is_durable_but_keeps_independent_support() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut original = document();
    let independent = source("src_second");
    original.sources.push(independent.clone());
    let mut supported = fact("fact_supported", "Supported twice.", &original.sources[0]);
    supported.sources.push(MemorySourceRef {
        source_id: independent.id,
        revision: independent.revision,
    });
    original.entries[0].facts.push(supported);
    save(&store, &original);
    let old_bytes = std::fs::read(store.root().join("MEMORY.md")).unwrap();
    let proposal = store
        .propose(
            &store.snapshot().unwrap().revision,
            "op_retract",
            MemoryChange::RetractSource {
                source_id: original.sources[0].id.clone(),
                revision: original.sources[0].revision.clone(),
            },
        )
        .unwrap();
    assert_eq!(
        store.commit(&proposal).unwrap().status,
        MemoryCommitStatus::Committed
    );
    std::fs::write(store.root().join("MEMORY.md"), old_bytes).unwrap();
    let snapshot = store.snapshot().unwrap();
    assert_eq!(snapshot.entries[0].facts.len(), 1);
    assert_eq!(snapshot.entries[0].facts[0].id, "fact_supported");
    assert_eq!(snapshot.entries[0].facts[0].sources.len(), 1);
}

#[test]
fn history_retention_is_bounded_and_versions_are_verified() {
    let temp = TempDir::new().unwrap();
    let mut store = store(&temp);
    store.limits.max_history_versions = 2;
    commit_initial(&store);
    for index in 0..5 {
        let proposal = update(
            &store,
            &format!("op_update_{index}"),
            &format!("Preference version {index}."),
        );
        assert_eq!(
            store.commit(&proposal).unwrap().status,
            MemoryCommitStatus::Committed
        );
    }
    let history = store.history().unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(
        std::fs::read_dir(store.root().join("memory/.saya-history"))
            .unwrap()
            .count(),
        2
    );
    let version = &history[0].version;
    std::fs::write(
        store
            .root()
            .join(format!("memory/.saya-history/{version}.md")),
        "corrupt",
    )
    .unwrap();
    assert_eq!(
        code(store.propose(
            &store.snapshot().unwrap().revision,
            "op_bad_history",
            MemoryChange::RestoreVersion {
                version: version.clone()
            }
        )),
        "memory_state"
    );
}

#[test]
fn cancellation_before_commit_has_zero_creations_and_lock_wait_is_bounded() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let proposal = update(&store, "op_cancelled", "Cancelled preference.");
    assert_eq!(
        code(store.commit_cancellable(&proposal, &AtomicBool::new(true))),
        "cancelled"
    );
    assert!(!store.root().exists());
    std::fs::create_dir(store.root()).unwrap();
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(store.root().join(".saya-memory.lock"))
        .unwrap();
    // SAFETY: flock is applied to this test's live owned regular descriptor.
    assert_eq!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let started = std::time::Instant::now();
    assert_eq!(code(store.commit(&proposal)), "memory_busy");
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert!(!store.root().join("MEMORY.md").exists());
}

#[test]
fn operation_ledger_capacity_fails_closed_without_dropping_retry_identity() {
    let temp = TempDir::new().unwrap();
    let mut store = store(&temp);
    store.limits.max_facts = 1;
    let first = commit_initial(&store);
    for index in 0..3 {
        let proposal = update(
            &store,
            &format!("op_update_{index}"),
            &format!("Version {index}."),
        );
        assert_eq!(
            store.commit(&proposal).unwrap().status,
            MemoryCommitStatus::Committed
        );
    }
    let full = update(&store, "op_over_capacity", "Beyond ledger capacity.");
    assert_eq!(code(store.commit(&full)), "memory_limit");
    assert_eq!(
        store.commit(&first).unwrap().status,
        MemoryCommitStatus::AlreadyCommitted
    );
}

#[test]
fn corrupt_state_and_cross_store_proposal_are_rejected_without_overwrite() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let proposal = update(&store, "op_reviewed", "A scoped update.");
    let other = GlobalMemoryStore::open(
        &temp.path().join("other"),
        store.workspace().clone(),
        store.limits(),
        Arc::new(SecretRegistry::default()),
    )
    .unwrap();
    assert_eq!(code(other.commit(&proposal)), "memory_scope");
    assert!(!other.root().exists());
    commit_initial(&store);
    let path = store.root().join(state::STATE_PATH);
    std::fs::write(&path, "malformed ledger").unwrap();
    assert_eq!(code(store.snapshot()), "memory_state");
    assert_eq!(std::fs::read_to_string(path).unwrap(), "malformed ledger");
}

#[test]
fn next_authorized_commit_cleans_recognized_crash_debris_only() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    commit_initial(&store);
    let proposal = update(&store, "op_staged", "A staged preference.");
    fail_once(FaultPoint::AfterStage);
    assert_eq!(code(store.commit(&proposal)), "memory_io");
    assert!(store.root().join(state::STAGE_PATH).exists());
    let temporary = ".saya-write-12345678901234567890123456789012.tmp";
    let path = store.root().join(temporary);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    std::fs::write(
        store.root().join("user-notes.md"),
        "Preserve user material.",
    )
    .unwrap();
    assert_eq!(
        store.commit(&proposal).unwrap().status,
        MemoryCommitStatus::Committed
    );
    assert!(!path.exists());
    assert!(!store.root().join(state::STAGE_PATH).exists());
    assert!(store.root().join("user-notes.md").exists());
}

#[test]
fn forgotten_source_descriptions_and_topics_stay_out_after_old_index_replay() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut original = document();
    original.sources[0].description = original.entries[0].facts[0].text.clone();
    original.entries[0].facts.push(fact(
        "fact_independent",
        "An unrelated preference.",
        &original.sources[0],
    ));
    original.entries[0]
        .resources
        .push("memory/editor.md".into());
    save(&store, &original);
    std::fs::create_dir(store.root().join("memory")).unwrap();
    std::fs::write(
        store.root().join("memory/editor.md"),
        "Details about the forgotten preference.",
    )
    .unwrap();
    let old = std::fs::read(store.root().join("MEMORY.md")).unwrap();
    let proposal = store
        .propose(
            &store.snapshot().unwrap().revision,
            "op_forget",
            MemoryChange::Forget {
                scope: MemoryScope::Global,
                fact_ids: vec!["fact_editor".into()],
            },
        )
        .unwrap();
    assert_eq!(
        store.commit(&proposal).unwrap().status,
        MemoryCommitStatus::Committed
    );
    let mut replay = store
        .parse_document(std::str::from_utf8(&old).unwrap())
        .unwrap();
    replay.entries[0].facts.remove(0);
    save(&store, &replay);
    let snapshot = store.snapshot().unwrap();
    assert!(snapshot.sources[0].description.is_empty());
    assert!(snapshot.resources.is_empty());
    assert_eq!(
        code(store.read_resource("memory/editor.md", None)),
        "memory_not_found"
    );
    // The ledger preserves suppression even if an external old index omits
    // the forgotten fact and carries an older topic link.
    assert_eq!(snapshot.entries[0].facts[0].id, "fact_independent");
}

#[test]
fn withdrawn_entry_title_stays_suppressed_after_an_older_index_is_replayed() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut original = document();
    original.entries[0].title = "An older source claim".into();
    let independent = source("src_independent");
    original.entries[0].facts.push(fact(
        "fact_independent",
        "An unrelated supported preference.",
        &independent,
    ));
    original.sources.push(independent);
    save(&store, &original);
    let retraction = store
        .propose(
            &store.snapshot().unwrap().revision,
            "op_retract_title",
            MemoryChange::RetractSource {
                source_id: original.sources[0].id.clone(),
                revision: original.sources[0].revision.clone(),
            },
        )
        .unwrap();
    assert_eq!(retraction.after_entries[0].title, "Memory entry");
    assert_eq!(
        store.commit(&retraction).unwrap().status,
        MemoryCommitStatus::Committed
    );
    original.entries[0].facts.remove(0);
    save(&store, &original);
    let snapshot = store.snapshot().unwrap();
    assert_eq!(snapshot.entries[0].title, "Memory entry");
    assert_eq!(snapshot.entries[0].facts[0].id, "fact_independent");
}

#[test]
fn forgotten_text_in_source_reference_and_observation_is_withheld_across_replay_and_rollback() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut original = document();
    let forgotten = original.entries[0].facts[0].text.clone();
    original.sources[0].reference = forgotten.clone();
    original.sources[0].observed_at = forgotten.clone();
    original.sources[0].description = forgotten.clone();
    original.entries[0].updated_at = forgotten.clone();
    original.entries[0].facts.push(fact(
        "fact_independent",
        "An independent current preference.",
        &original.sources[0],
    ));
    save(&store, &original);
    let old_version = digest(&std::fs::read(store.root().join("MEMORY.md")).unwrap());
    let forget = store
        .propose(
            &store.snapshot().unwrap().revision,
            "op_forget_metadata",
            MemoryChange::Forget {
                scope: MemoryScope::Global,
                fact_ids: vec!["fact_editor".into()],
            },
        )
        .unwrap();
    assert_eq!(
        store.commit(&forget).unwrap().status,
        MemoryCommitStatus::Committed
    );
    let assert_withheld = |snapshot: &MemorySnapshot| {
        assert_eq!(snapshot.entries[0].facts[0].id, "fact_independent");
        assert_eq!(snapshot.entries[0].updated_at, "withheld");
        assert_eq!(snapshot.sources[0].id, original.sources[0].id);
        assert_eq!(snapshot.sources[0].revision, original.sources[0].revision);
        assert_eq!(snapshot.sources[0].reference, "withheld");
        assert_eq!(snapshot.sources[0].observed_at, "withheld");
        assert!(snapshot.sources[0].description.is_empty());
        assert!(snapshot.sources[0].metadata_withheld);
        assert!(
            !serde_json::to_string(snapshot)
                .unwrap()
                .contains(&forgotten)
        );
    };
    let current = store.snapshot().unwrap();
    assert_withheld(&current);
    assert_eq!(store.snapshot().unwrap(), current);
    let mut replay = original.clone();
    replay.entries[0].facts.remove(0);
    save(&store, &replay);
    assert_withheld(&store.snapshot().unwrap());
    let rollback = store
        .propose(
            &store.snapshot().unwrap().revision,
            "op_restore_metadata",
            MemoryChange::RestoreVersion {
                version: old_version,
            },
        )
        .unwrap();
    assert_eq!(
        store.commit(&rollback).unwrap().status,
        MemoryCommitStatus::Committed
    );
    assert_withheld(&store.snapshot().unwrap());
}

#[test]
fn host_request_identity_survives_later_updates_and_forgetting_without_reapplying_facts() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let request_digest = digest(b"normalized request and stable evidence");
    let first = update(&store, "op_host_bound", "The original saved preference.")
        .bind_host_request(&request_digest)
        .unwrap();
    assert_eq!(
        store.commit(&first).unwrap().status,
        MemoryCommitStatus::Committed
    );
    let later = update(&store, "op_later", "A later corrected preference.");
    assert_eq!(
        store.commit(&later).unwrap().status,
        MemoryCommitStatus::Committed
    );
    let retry = store.commit(&first).unwrap();
    assert_eq!(retry.status, MemoryCommitStatus::AlreadyCommitted);
    assert_eq!(
        retry.receipt.host_request_digest.as_deref(),
        Some(request_digest.as_str())
    );
    assert_eq!(
        store.snapshot().unwrap().entries[0].facts[0].text,
        "A later corrected preference."
    );
    let forget = store
        .propose(
            &store.snapshot().unwrap().revision,
            "op_forget_bound",
            MemoryChange::Forget {
                scope: MemoryScope::Global,
                fact_ids: vec!["fact_editor".into()],
            },
        )
        .unwrap();
    assert_eq!(
        store.commit(&forget).unwrap().status,
        MemoryCommitStatus::Committed
    );
    let current_revision = store.snapshot().unwrap().revision;
    let retry = store.commit(&first).unwrap();
    assert_eq!(retry.status, MemoryCommitStatus::AlreadyCommitted);
    assert_eq!(retry.revision, Some(current_revision));
    assert!(store.snapshot().unwrap().entries.is_empty());
    let changed_request = first
        .clone()
        .bind_host_request(&digest(b"different normalized request"))
        .unwrap();
    assert_eq!(
        code(store.commit(&changed_request)),
        "memory_idempotency_conflict"
    );
}

#[test]
fn journal_recovery_preserves_host_request_binding_and_legacy_receipts_remain_readable() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let original = commit_initial(&store);
    let legacy = store.operation_receipt("op_initial").unwrap().unwrap();
    assert!(legacy.host_request_digest.is_none());
    let encoded_legacy = serde_json::to_value(&legacy).unwrap();
    assert!(encoded_legacy.get("host_request_digest").is_none());
    assert!(
        serde_json::from_value::<MemoryOperationReceipt>(encoded_legacy)
            .unwrap()
            .host_request_digest
            .is_none()
    );
    let request_digest = digest(b"fault-safe host request");
    let change = update(&store, "op_host_pending", "A journaled preference.")
        .change()
        .clone();
    let proposal = store
        .propose_host_request(
            &store.snapshot().unwrap().revision,
            "op_host_pending",
            &request_digest,
            change,
        )
        .unwrap();
    fail_once(FaultPoint::AfterIndex);
    assert_eq!(
        store.commit(&proposal).unwrap().status,
        MemoryCommitStatus::Unknown
    );
    let recovered = store.recover().unwrap().unwrap();
    assert_eq!(
        recovered.host_request_digest.as_deref(),
        Some(request_digest.as_str())
    );
    assert_eq!(
        store.commit(&proposal).unwrap().status,
        MemoryCommitStatus::AlreadyCommitted
    );
    assert_eq!(
        store.commit(&original).unwrap().status,
        MemoryCommitStatus::AlreadyCommitted
    );
    let unbound = update(&store, "op_host_pending", "A journaled preference.");
    assert_eq!(code(store.commit(&unbound)), "memory_idempotency_conflict");
}

#[test]
fn review_redaction_uses_current_store_credentials_without_initializing_memory() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let secret = "review-protected-fixture".to_owned();
    store
        .secrets
        .register(std::slice::from_ref(&secret))
        .unwrap();
    let value = serde_json::json!({"excerpt": format!("User supplied {secret} with an unrelated preference.")});
    let redacted = store.redact_review_value(value).unwrap();
    assert!(!serde_json::to_string(&redacted).unwrap().contains(&secret));
    assert!(!store.root().exists());
    let mut original = document();
    original.entries[0].facts[0].text = "An unrelated safe preference.".into();
    assert_eq!(
        code(store.propose(
            &store.snapshot().unwrap().revision,
            &format!("op_{secret}"),
            MemoryChange::Upsert {
                entry: original.entries[0].clone(),
                sources: original.sources.clone()
            }
        )),
        "sensitive_data"
    );
    assert!(!store.root().exists());
}
