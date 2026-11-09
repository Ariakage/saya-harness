use super::*;
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
};

struct Fixture {
    _root: tempfile::TempDir,
    path: PathBuf,
    store: CheckpointStore,
    bound: CheckpointWorkspace,
    session: SessionId,
}
impl Fixture {
    fn new() -> Self {
        Self::with_limits(CheckpointLimits::default())
    }
    fn with_limits(limits: CheckpointLimits) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("workspace");
        fs::create_dir(&path).unwrap();
        let store = CheckpointStore::open(
            root.path().join("checkpoints"),
            Arc::new(SecretRegistry::default()),
            limits,
        )
        .unwrap();
        let session = SessionId::new();
        let bound = store.bind_workspace(&path, session, &[]).unwrap();
        Self {
            _root: root,
            path,
            store,
            bound,
            session,
        }
    }
    fn source(&self) -> CheckpointSource {
        CheckpointSource {
            session_id: self.session,
            run_id: RunId::new(),
            revision: 1,
            label: "before edit".into(),
        }
    }
    fn write(&self, path: &str, text: &str) {
        fs::write(self.path.join(path), text).unwrap();
    }
    fn capture(&self, names: &[&str]) -> CheckpointSummary {
        self.bound
            .capture(&paths(names), self.source(), &stop())
            .unwrap()
    }
    fn prepare(&self, checkpoint: &CheckpointSummary, names: &[&str]) -> PreparedRestore {
        self.bound
            .prepare_restore(&checkpoint.token, &paths(names), self.source(), &stop())
            .unwrap()
    }
}
fn paths(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).into()).collect()
}
fn stop() -> AtomicBool {
    AtomicBool::new(false)
}
fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

#[test]
fn snapshot_preparation_publishes_only_approved_observations_and_preserves_previous_baselines() {
    let fixture = Fixture::new();
    fixture.write("selected", "approved original\n");
    let prepared = fixture
        .bound
        .prepare_capture(&paths(&["selected"]), fixture.source(), &stop())
        .unwrap();
    assert_eq!(prepared.paths(), paths(&["selected"]));
    assert_eq!(prepared.observations().len(), 1);
    assert!(fixture.bound.list().unwrap().is_empty());
    fixture.write("selected", "changed after approval preview\n");
    assert_eq!(
        fixture
            .bound
            .publish_snapshot(prepared, &stop())
            .unwrap_err()
            .0
            .code,
        "checkpoint_conflict"
    );
    assert!(fixture.bound.list().unwrap().is_empty());
    let baseline = fixture.capture(&["selected"]);
    fixture.write("selected", "accepted candidate\n");
    let prepared = fixture
        .bound
        .prepare_accept(
            &baseline.token,
            &paths(&["selected"]),
            fixture.source(),
            &stop(),
        )
        .unwrap();
    fixture.write("selected", "changed again\n");
    assert_eq!(
        fixture
            .bound
            .publish_snapshot(prepared, &stop())
            .unwrap_err()
            .0
            .code,
        "checkpoint_conflict"
    );
    assert_eq!(fixture.bound.list().unwrap().len(), 1);
    assert_eq!(read(&fixture.path.join("selected")), "changed again\n");
}

#[test]
fn prepared_snapshots_bind_the_session_and_recheck_current_credentials_before_publication() {
    let fixture = Fixture::new();
    fixture.write("selected", "newly classified credential\n");
    let prepared = fixture
        .bound
        .prepare_capture(&paths(&["selected"]), fixture.source(), &stop())
        .unwrap();
    let other = fixture
        .store
        .bind_workspace(&fixture.path, SessionId::new(), &[])
        .unwrap();
    assert!(other.publish_snapshot(prepared, &stop()).is_err());
    assert!(fixture.bound.list().unwrap().is_empty());
    let other_store = CheckpointStore::open(
        fixture._root.path().join("other-checkpoints"),
        Arc::new(SecretRegistry::default()),
        CheckpointLimits::default(),
    )
    .unwrap();
    let other = other_store
        .bind_workspace(&fixture.path, fixture.session, &[])
        .unwrap();
    let prepared = fixture
        .bound
        .prepare_capture(&paths(&["selected"]), fixture.source(), &stop())
        .unwrap();
    assert!(other.publish_snapshot(prepared, &stop()).is_err());
    assert!(other.list().unwrap().is_empty());
    let prepared = fixture
        .bound
        .prepare_capture(&paths(&["selected"]), fixture.source(), &stop())
        .unwrap();
    fixture
        .store
        .inner
        .secrets
        .register(&["newly classified credential".into()])
        .unwrap();
    assert!(fixture.bound.publish_snapshot(prepared, &stop()).is_err());
    assert!(fixture.bound.list().unwrap().is_empty());
}

#[test]
fn ordinary_files_support_diff_restore_creation_deletion_modes_and_idempotence() {
    let fixture = Fixture::new();
    fixture.write("edited", "original\n");
    fixture.write("deleted", "restore this\n");
    fs::set_permissions(
        fixture.path.join("edited"),
        fs::Permissions::from_mode(0o640),
    )
    .unwrap();
    let checkpoint = fixture.capture(&["edited", "deleted", "created"]);
    fixture.write("edited", "agent edit\n");
    fixture.write("created", "new file\n");
    fs::remove_file(fixture.path.join("deleted")).unwrap();
    let diff = fixture.bound.diff(&checkpoint.token, &stop()).unwrap();
    assert_eq!(
        diff.files
            .iter()
            .find(|file| file.path == "created")
            .unwrap()
            .change,
        ChangeKind::Added
    );
    assert_eq!(
        diff.files
            .iter()
            .find(|file| file.path == "deleted")
            .unwrap()
            .change,
        ChangeKind::Deleted
    );
    assert!(
        diff.files
            .iter()
            .find(|file| file.path == "edited")
            .unwrap()
            .unified_diff
            .as_ref()
            .unwrap()
            .contains("+agent edit")
    );
    let prepared = fixture.prepare(&checkpoint, &["edited", "deleted", "created"]);
    let mut approved = Vec::new();
    let outcome = fixture
        .bound
        .apply(&prepared, &stop(), |operation| {
            approved.push(operation.path.clone());
            Ok(())
        })
        .unwrap();
    assert_eq!(outcome.status, RestoreStatus::Completed);
    assert_eq!(approved.len(), 3);
    assert!(outcome.files.iter().all(|file| file.publication_observed));
    assert_eq!(read(&fixture.path.join("edited")), "original\n");
    assert_eq!(read(&fixture.path.join("deleted")), "restore this\n");
    assert!(!fixture.path.join("created").exists());
    assert_eq!(
        fs::metadata(fixture.path.join("edited")).unwrap().mode() & 0o777,
        0o640
    );
    fixture.write("edited", "later external edit");
    assert_eq!(
        fixture
            .bound
            .apply(&prepared, &stop(), |_| panic!(
                "completed restore must not replay"
            ))
            .unwrap()
            .status,
        RestoreStatus::Completed
    );
    assert_eq!(read(&fixture.path.join("edited")), "later external edit");
    assert_eq!(
        fs::metadata(fixture.store.protected_path()).unwrap().mode() & 0o777,
        0o700
    );
}

#[test]
fn git_directory_and_worktree_marker_keep_index_head_and_preexisting_dirty_bytes() {
    for worktree in [false, true] {
        let fixture = Fixture::new();
        if worktree {
            fixture.write(".git", "gitdir: /outside/managed/repository\n");
        } else {
            fs::create_dir(fixture.path.join(".git")).unwrap();
            fs::write(fixture.path.join(".git/index"), "staged index bytes").unwrap();
            fs::write(fixture.path.join(".git/HEAD"), "ref: refs/heads/work\n").unwrap();
        }
        let bound = fixture
            .store
            .bind_workspace(&fixture.path, fixture.session, &[])
            .unwrap();
        assert_eq!(
            bound.kind(),
            if worktree {
                WorkspaceKind::GitWorktree
            } else {
                WorkspaceKind::GitDirectory
            }
        );
        fixture.write("tracked", "preexisting user dirty contents\n");
        let checkpoint = bound
            .capture(&paths(&["tracked"]), fixture.source(), &stop())
            .unwrap();
        fixture.write("tracked", "agent edit\n");
        let prepared = bound
            .prepare_restore(
                &checkpoint.token,
                &paths(&["tracked"]),
                fixture.source(),
                &stop(),
            )
            .unwrap();
        assert_eq!(
            bound.apply(&prepared, &stop(), |_| Ok(())).unwrap().status,
            RestoreStatus::Completed
        );
        assert_eq!(
            read(&fixture.path.join("tracked")),
            "preexisting user dirty contents\n"
        );
        if worktree {
            assert_eq!(
                read(&fixture.path.join(".git")),
                "gitdir: /outside/managed/repository\n"
            );
        } else {
            assert_eq!(read(&fixture.path.join(".git/index")), "staged index bytes");
            assert_eq!(
                read(&fixture.path.join(".git/HEAD")),
                "ref: refs/heads/work\n"
            );
        }
        assert!(
            bound
                .capture(&paths(&[".git/index"]), fixture.source(), &stop())
                .is_err()
        );
    }
}

#[test]
fn accepting_selected_paths_creates_a_new_immutable_baseline() {
    let fixture = Fixture::new();
    fixture.write("a", "first a");
    fixture.write("b", "first b");
    let checkpoint = fixture.capture(&["a", "b"]);
    fixture.write("a", "second a");
    fixture.write("b", "second b");
    let accepted = fixture
        .bound
        .accept(&checkpoint.token, &paths(&["a"]), fixture.source(), &stop())
        .unwrap();
    assert_ne!(checkpoint.token, accepted.token);
    assert_eq!(accepted.parent, Some(checkpoint.token.clone()));
    let current = fixture.bound.diff(&accepted.token, &stop()).unwrap();
    assert_eq!(current.files[0].change, ChangeKind::Unchanged);
    assert_eq!(current.files[1].change, ChangeKind::Modified);
    assert!(
        fixture
            .bound
            .diff(&checkpoint.token, &stop())
            .unwrap()
            .files
            .iter()
            .all(|file| file.change == ChangeKind::Modified)
    );
    assert_eq!(fixture.bound.list().unwrap().len(), 2);
}

#[test]
fn conflicts_before_apply_and_after_approval_preserve_external_edits() {
    let fixture = Fixture::new();
    fixture.write("a", "original");
    let checkpoint = fixture.capture(&["a"]);
    fixture.write("a", "agent edit");
    let prepared = fixture.prepare(&checkpoint, &["a"]);
    fixture.write("a", "external edit");
    let outcome = fixture
        .bound
        .apply(&prepared, &stop(), |_| {
            panic!("preflight conflict must not authorize writes")
        })
        .unwrap();
    assert_eq!(outcome.status, RestoreStatus::Conflict);
    assert!(!outcome.has_applied_files());
    assert_eq!(read(&fixture.path.join("a")), "external edit");
    let prepared = fixture.prepare(&checkpoint, &["a"]);
    let outcome = fixture
        .bound
        .apply(&prepared, &stop(), |_| {
            fixture.write("a", "edited during approval");
            Ok(())
        })
        .unwrap();
    assert_eq!(outcome.status, RestoreStatus::Conflict);
    assert_eq!(read(&fixture.path.join("a")), "edited during approval");
}

#[test]
fn identical_content_replacement_is_still_a_version_conflict() {
    let fixture = Fixture::new();
    fixture.write("a", "original");
    let checkpoint = fixture.capture(&["a"]);
    fixture.write("a", "agent edit");
    let prepared = fixture.prepare(&checkpoint, &["a"]);
    fixture.write("replacement", "agent edit");
    fs::rename(fixture.path.join("replacement"), fixture.path.join("a")).unwrap();
    let outcome = fixture
        .bound
        .apply(&prepared, &stop(), |_| {
            panic!("inode replacement must conflict")
        })
        .unwrap();
    assert_eq!(outcome.status, RestoreStatus::Conflict);
    assert_eq!(read(&fixture.path.join("a")), "agent edit");
}

#[test]
fn current_authorization_can_reject_all_or_leave_an_honest_partial_restore() {
    let fixture = Fixture::new();
    fixture.write("a", "original a");
    fixture.write("b", "original b");
    let checkpoint = fixture.capture(&["a", "b"]);
    fixture.write("a", "agent a");
    fixture.write("b", "agent b");
    let denied = fixture.prepare(&checkpoint, &["a", "b"]);
    let outcome = fixture
        .bound
        .apply(&denied, &stop(), |_| Err(error("plan_read_only", "Denied")))
        .unwrap();
    assert_eq!(outcome.status, RestoreStatus::Rejected);
    assert!(!outcome.has_applied_files());
    assert_eq!(read(&fixture.path.join("a")), "agent a");
    let partial = fixture.prepare(&checkpoint, &["a", "b"]);
    let outcome = fixture
        .bound
        .apply(&partial, &stop(), |operation| {
            if operation.path == "b" {
                Err(error("stale_approval", "Denied"))
            } else {
                Ok(())
            }
        })
        .unwrap();
    assert_eq!(outcome.status, RestoreStatus::Partial);
    assert!(outcome.has_applied_files());
    assert_eq!(read(&fixture.path.join("a")), "original a");
    assert_eq!(read(&fixture.path.join("b")), "agent b");
}

#[test]
fn cancellation_between_files_preserves_observed_changes_and_stops_remaining_writes() {
    let fixture = Fixture::new();
    fixture.write("a", "original a");
    fixture.write("b", "original b");
    let checkpoint = fixture.capture(&["a", "b"]);
    fixture.write("a", "agent a");
    fixture.write("b", "agent b");
    let prepared = fixture.prepare(&checkpoint, &["a", "b"]);
    let cancel = stop();
    let outcome = fixture
        .bound
        .apply(&prepared, &cancel, |operation| {
            if operation.path == "b" {
                cancel.store(true, Ordering::SeqCst);
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(outcome.status, RestoreStatus::Partial);
    assert_eq!(read(&fixture.path.join("a")), "original a");
    assert_eq!(read(&fixture.path.join("b")), "agent b");
    assert_eq!(outcome.error.unwrap().code, "cancelled");
    assert!(
        fixture
            .bound
            .capture(&paths(&["a"]), fixture.source(), &cancel)
            .is_err()
    );
}

#[test]
fn final_observation_reports_external_changes_to_an_earlier_published_file() {
    let fixture = Fixture::new();
    fixture.write("a", "original a");
    fixture.write("b", "original b");
    let checkpoint = fixture.capture(&["a", "b"]);
    fixture.write("a", "agent a");
    fixture.write("b", "agent b");
    let prepared = fixture.prepare(&checkpoint, &["a", "b"]);
    let outcome = fixture
        .bound
        .apply(&prepared, &stop(), |operation| {
            if operation.path == "b" {
                fixture.write("a", "external later edit");
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(outcome.status, RestoreStatus::Partial);
    assert!(outcome.files[0].publication_observed);
    assert_eq!(outcome.files[0].status, FileOutcomeStatus::Conflict);
    assert_eq!(read(&fixture.path.join("a")), "external later edit");
}

#[test]
fn journal_failure_retains_observed_results_and_recovery_never_replays() {
    let fixture = Fixture::new();
    fixture.write("a", "original a");
    fixture.write("b", "original b");
    let checkpoint = fixture.capture(&["a", "b"]);
    fixture.write("a", "agent a");
    fixture.write("b", "agent b");
    let prepared = fixture.prepare(&checkpoint, &["a", "b"]);
    let failure = fixture
        .bound
        .apply(&prepared, &stop(), |operation| {
            if operation.path == "b" {
                fixture
                    .store
                    .inner
                    .disk
                    .fail_journal
                    .store(true, Ordering::SeqCst);
            }
            Ok(())
        })
        .unwrap_err();
    assert_eq!(failure.0.code, "checkpoint_storage");
    assert_eq!(
        failure.2.as_ref().unwrap().data.as_ref().unwrap()["files"][0]["publication_observed"],
        true
    );
    assert_eq!(read(&fixture.path.join("a")), "original a");
    assert_eq!(read(&fixture.path.join("b")), "agent b");
    let observed = fixture.bound.recover(&stop()).unwrap();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].status, RestoreStatus::Unknown);
    assert_eq!(
        observed[0].files[0].status,
        FileOutcomeStatus::ObservedDesired
    );
    assert_eq!(
        observed[0].files[1].status,
        FileOutcomeStatus::ObservedBefore
    );
    assert!(
        fixture
            .bound
            .apply(&prepared, &stop(), |_| panic!(
                "unknown operation must not replay"
            ))
            .is_err()
    );
    assert_eq!(read(&fixture.path.join("b")), "agent b");
}

#[test]
fn secrets_are_rejected_before_snapshot_persistence_and_diffs_are_redacted() {
    let fixture = Fixture::new();
    let secret = "fixture-protected-credential-12345";
    fixture
        .store
        .inner
        .secrets
        .register(&[secret.into()])
        .unwrap();
    fixture.write("a", "public data");
    fixture.write("b", secret);
    let failure = fixture
        .bound
        .capture(&paths(&["a", "b"]), fixture.source(), &stop())
        .unwrap_err();
    assert_eq!(failure.0.code, "sensitive_data");
    assert!(!failure.to_string().contains(secret));
    assert_eq!(
        fs::read_dir(fixture.store.protected_path().join("blobs"))
            .unwrap()
            .count(),
        0
    );
    assert_eq!(
        fs::read_dir(fixture.store.protected_path().join("snapshots"))
            .unwrap()
            .count(),
        0
    );
    fixture.write("b", "old public data");
    let checkpoint = fixture.capture(&["b"]);
    fixture.write("b", secret);
    let diff = fixture.bound.diff(&checkpoint.token, &stop()).unwrap();
    assert!(!serde_json::to_string(&diff).unwrap().contains(secret));
    assert!(
        fixture
            .bound
            .prepare_restore(&checkpoint.token, &paths(&["b"]), fixture.source(), &stop())
            .is_err()
    );
    let mut source = fixture.source();
    source.label = secret.into();
    assert!(
        fixture
            .bound
            .capture(&paths(&["a"]), source, &stop())
            .is_err()
    );
}

#[test]
fn unsafe_paths_links_control_directories_and_opaque_files_are_rejected() {
    let fixture = Fixture::new();
    fixture.write("plain", "ordinary");
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("file"), "outside").unwrap();
    symlink(outside.path(), fixture.path.join("linked-parent")).unwrap();
    symlink(
        outside.path().join("file"),
        fixture.path.join("linked-file"),
    )
    .unwrap();
    fs::hard_link(outside.path().join("file"), fixture.path.join("hardlinked")).unwrap();
    fs::write(fixture.path.join("opaque"), [0u8, 255, 2]).unwrap();
    for path in [
        "../outside",
        "/absolute",
        "dir\\file",
        "a/./b",
        "a//b",
        ".git/index",
        ".GIT/HEAD",
        ".saya/local/config",
        ".SAYA/LOCAL/config",
        "linked-parent/file",
        "linked-file",
        "hardlinked",
        "opaque",
    ] {
        assert!(
            fixture
                .bound
                .capture(&paths(&[path]), fixture.source(), &stop())
                .is_err(),
            "{path}"
        );
    }
    for selected in [
        paths(&["plain", "PLAIN"]),
        paths(&["caf\u{e9}", "cafe\u{301}"]),
        paths(&["dir", "dir/file"]),
    ] {
        assert!(
            fixture
                .bound
                .capture(&selected, fixture.source(), &stop())
                .is_err()
        );
    }
    assert_eq!(read(&outside.path().join("file")), "outside");
}

#[test]
fn protected_paths_root_replacement_and_foreign_sessions_fail_closed() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.path.join("private")).unwrap();
    fixture.write("private/file", "protected");
    let protected = fixture
        .store
        .bind_workspace(
            &fixture.path,
            fixture.session,
            &[fixture.path.join("private")],
        )
        .unwrap();
    assert!(
        protected
            .capture(&paths(&["private/file"]), fixture.source(), &stop())
            .is_err()
    );
    assert!(
        protected
            .capture(&paths(&["PRIVATE/file"]), fixture.source(), &stop())
            .is_err()
    );
    fixture.write("plain", "original");
    let checkpoint = fixture.capture(&["plain"]);
    let foreign = fixture
        .store
        .bind_workspace(&fixture.path, SessionId::new(), &[])
        .unwrap();
    assert!(foreign.diff(&checkpoint.token, &stop()).is_err());
    assert!(foreign.list().unwrap().is_empty());
    let mut bad = checkpoint.token.clone();
    bad.sha256 = "a".repeat(64);
    assert!(fixture.bound.diff(&bad, &stop()).is_err());
    let moved = fixture.path.with_file_name("moved");
    fs::rename(&fixture.path, &moved).unwrap();
    fs::create_dir(&fixture.path).unwrap();
    fixture.write("plain", "new root");
    assert!(fixture.bound.diff(&checkpoint.token, &stop()).is_err());
    assert_eq!(read(&fixture.path.join("plain")), "new root");
}

#[test]
fn bounded_diff_and_storage_limits_return_explicit_results() {
    let fixture = Fixture::with_limits(CheckpointLimits {
        max_files: 1,
        max_checkpoints: 1,
        max_diff_bytes: 24,
        ..CheckpointLimits::default()
    });
    fixture.write("a", "original\n");
    fixture.write("b", "other");
    assert!(
        fixture
            .bound
            .capture(&paths(&["a", "b"]), fixture.source(), &stop())
            .is_err()
    );
    let checkpoint = fixture.capture(&["a"]);
    fixture.write("a", &"changed\n".repeat(100));
    let diff = fixture.bound.diff(&checkpoint.token, &stop()).unwrap();
    assert!(diff.preview_truncated);
    assert!(diff.files[0].unified_diff.as_ref().unwrap().len() <= 24);
    fixture.write("a", &"long line\n".repeat(3000));
    let diff = fixture.bound.diff(&checkpoint.token, &stop()).unwrap();
    assert!(diff.preview_truncated);
    assert!(diff.files[0].unified_diff.is_none());
    assert!(
        fixture
            .bound
            .capture(&paths(&["a"]), fixture.source(), &stop())
            .is_err()
    );
    assert_eq!(fixture.bound.list().unwrap().len(), 1);
}

#[test]
fn private_store_links_and_corrupt_blobs_fail_before_restoring_workspace_bytes() {
    let fixture = Fixture::new();
    fixture.write("a", "original");
    let checkpoint = fixture.capture(&["a"]);
    fixture.write("a", "agent edit");
    let digest = hash(b"original");
    let blob = fixture
        .store
        .protected_path()
        .join("blobs")
        .join(format!("{digest}.bin"));
    fs::set_permissions(&blob, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&blob, "tampered").unwrap();
    assert!(
        fixture
            .bound
            .prepare_restore(&checkpoint.token, &paths(&["a"]), fixture.source(), &stop())
            .is_err()
    );
    assert_eq!(read(&fixture.path.join("a")), "agent edit");
    fs::remove_file(&blob).unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("data"), "original").unwrap();
    symlink(outside.path().join("data"), &blob).unwrap();
    assert!(fixture.bound.diff(&checkpoint.token, &stop()).is_err());
    fs::remove_file(&blob).unwrap();
    fs::hard_link(outside.path().join("data"), &blob).unwrap();
    assert!(fixture.bound.diff(&checkpoint.token, &stop()).is_err());
    let linked_root = outside.path().join("linked-root");
    symlink(fixture.store.protected_path(), &linked_root).unwrap();
    assert!(
        CheckpointStore::open(
            linked_root,
            Arc::new(SecretRegistry::default()),
            CheckpointLimits::default()
        )
        .is_err()
    );
    assert_eq!(read(&outside.path().join("data")), "original");
}

#[test]
fn replaced_parent_and_store_root_are_detected_without_following_new_locations() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.path.join("dir")).unwrap();
    fixture.write("dir/a", "original");
    let checkpoint = fixture.capture(&["dir/a"]);
    fixture.write("dir/a", "agent edit");
    let prepared = fixture.prepare(&checkpoint, &["dir/a"]);
    let outcome = fixture
        .bound
        .apply(&prepared, &stop(), |_| {
            fs::rename(fixture.path.join("dir"), fixture.path.join("moved-dir")).unwrap();
            fs::create_dir(fixture.path.join("dir")).unwrap();
            fixture.write("dir/a", "external replacement");
            Ok(())
        })
        .unwrap();
    assert_eq!(outcome.status, RestoreStatus::Conflict);
    assert!(!outcome.has_applied_files());
    assert_eq!(read(&fixture.path.join("dir/a")), "external replacement");
    assert_eq!(read(&fixture.path.join("moved-dir/a")), "agent edit");
    let root = fixture.store.protected_path().to_owned();
    let moved = root.with_file_name("moved-checkpoints");
    fs::rename(&root, moved).unwrap();
    fs::create_dir(&root).unwrap();
    assert!(fixture.bound.list().is_err());
    assert_eq!(fs::read_dir(root).unwrap().count(), 0);
}
