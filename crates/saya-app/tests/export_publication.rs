use saya_app::{
    export::{publish, publish_session},
    interaction::RunCancellation,
};
use saya_core::{boundary::Cancellation, contract::WorkMode};
use std::{
    future::Future,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
};

#[test]
fn publishes_private_verified_file_and_preserves_existing_targets() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("export.json");
    let receipt = publish(
        &target,
        b"complete",
        temp.path(),
        WorkMode::Work,
        &RunCancellation::new(),
    )
    .unwrap();
    assert_eq!(receipt["bytes"], 8);
    assert_eq!(receipt["sha256"].as_str().unwrap().len(), 64);
    assert_eq!(
        std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        publish(
            &target,
            b"replacement",
            temp.path(),
            WorkMode::Work,
            &RunCancellation::new()
        )
        .unwrap_err()
        .0
        .code,
        "export_exists"
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"complete");
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
}

#[test]
fn rejects_symlinks_and_plan_workspace_targets() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let target = temp.path().join("link");
    std::os::unix::fs::symlink(workspace.join("target"), &target).unwrap();
    assert_eq!(
        publish(
            &target,
            b"secret",
            &workspace,
            WorkMode::Work,
            &RunCancellation::new()
        )
        .unwrap_err()
        .0
        .code,
        "export_exists"
    );
    assert!(!workspace.join("target").exists());
    assert_eq!(
        publish(
            &workspace.join("plan.md"),
            b"text",
            &workspace,
            WorkMode::Plan,
            &RunCancellation::new()
        )
        .unwrap_err()
        .0
        .code,
        "plan_write_denied"
    );
    publish(
        &temp.path().join("outside.md"),
        b"text",
        &workspace,
        WorkMode::Plan,
        &RunCancellation::new(),
    )
    .unwrap();
    assert_eq!(std::fs::read_dir(workspace).unwrap().count(), 0);
}

#[test]
fn plan_session_export_protects_current_and_saved_workspaces_before_staging() {
    let root = tempfile::tempdir().unwrap();
    let current = root.path().join("current");
    let saved = root.path().join("saved");
    std::fs::create_dir(&current).unwrap();
    std::fs::create_dir(&saved).unwrap();
    let alias = root.path().join("saved-alias");
    std::os::unix::fs::symlink(&saved, &alias).unwrap();
    for target in [
        current.join("file.md"),
        saved.join("file.md"),
        alias.join("file.md"),
    ] {
        assert_eq!(
            publish_session(
                &target,
                b"content",
                &current,
                &saved,
                WorkMode::Plan,
                &RunCancellation::new()
            )
            .unwrap_err()
            .0
            .code,
            "plan_write_denied"
        );
    }
    assert_eq!(std::fs::read_dir(&current).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(&saved).unwrap().count(), 0);
    publish_session(
        &root.path().join("outside.md"),
        b"content",
        &current,
        &saved,
        WorkMode::Plan,
        &RunCancellation::new(),
    )
    .unwrap();
    publish_session(
        &saved.join("work.md"),
        b"content",
        &current,
        &saved,
        WorkMode::Work,
        &RunCancellation::new(),
    )
    .unwrap();
}
struct DuringPublication {
    count: AtomicUsize,
    target: PathBuf,
    cancel_at: usize,
    create_at: usize,
}
impl Cancellation for DuringPublication {
    fn is_cancelled(&self) -> bool {
        let count = self.count.fetch_add(1, Ordering::SeqCst) + 1;
        if count == self.create_at {
            std::fs::write(&self.target, b"winner").unwrap();
        }
        count >= self.cancel_at
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}
#[test]
fn cancellation_mid_write_cleans_staging_and_commit_races_preserve_winner() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("export.json");
    let cancel = DuringPublication {
        count: AtomicUsize::new(0),
        target: target.clone(),
        cancel_at: 3,
        create_at: usize::MAX,
    };
    assert_eq!(
        publish(
            &target,
            &vec![1; 131072],
            temp.path(),
            WorkMode::Work,
            &cancel
        )
        .unwrap_err()
        .0
        .code,
        "cancelled"
    );
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    let race = DuringPublication {
        count: AtomicUsize::new(0),
        target: target.clone(),
        cancel_at: usize::MAX,
        create_at: 3,
    };
    assert_eq!(
        publish(&target, b"loser", temp.path(), WorkMode::Work, &race)
            .unwrap_err()
            .0
            .code,
        "export_exists"
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"winner");
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
}

struct ReplaceParent {
    checks: AtomicUsize,
    replace_at: usize,
    parent: PathBuf,
    displaced: PathBuf,
    workspace: PathBuf,
}
impl Cancellation for ReplaceParent {
    fn is_cancelled(&self) -> bool {
        let check = self.checks.fetch_add(1, Ordering::SeqCst) + 1;
        if check == self.replace_at {
            std::fs::rename(&self.parent, &self.displaced).unwrap();
            std::os::unix::fs::symlink(&self.workspace, &self.parent).unwrap();
        }
        false
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}

#[test]
fn plan_parent_replacement_cannot_redirect_staging_or_publication_into_workspace() {
    for replace_at in [2, 3] {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let parent = temp.path().join("exports");
        let displaced = temp.path().join("displaced");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir(&parent).unwrap();
        let replace = ReplaceParent {
            checks: AtomicUsize::new(0),
            replace_at,
            parent: parent.clone(),
            displaced: displaced.clone(),
            workspace: workspace.clone(),
        };
        let error = publish(
            &parent.join("plan.md"),
            b"plan export",
            &workspace,
            WorkMode::Plan,
            &replace,
        )
        .unwrap_err();
        assert_eq!(error.0.code, "export_path_changed");
        assert!(std::fs::symlink_metadata(&parent).unwrap().is_symlink());
        assert_eq!(std::fs::read_dir(&workspace).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(&displaced).unwrap().count(), 0);
        assert!(!parent.join("plan.md").exists());
    }
}

#[test]
fn publication_write_failure_child() {
    if std::env::var_os("SAYA_PUBLICATION_LIMIT_CHILD").is_none() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("export.json");
    // SAFETY: this isolated test process ignores the file-size signal so write
    // returns an error. The parent process keeps its original resource limits.
    let previous = unsafe { libc::signal(libc::SIGXFSZ, libc::SIG_IGN) };
    assert_ne!(previous, libc::SIG_ERR);
    let limit = libc::rlimit {
        rlim_cur: 8,
        rlim_max: 8,
    };
    // SAFETY: limit is initialized and lives until setrlimit returns.
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_FSIZE, &limit) }, 0);
    let error = publish(
        &target,
        b"more than eight bytes",
        temp.path(),
        WorkMode::Work,
        &RunCancellation::new(),
    )
    .unwrap_err();
    assert_eq!(error.0.code, "export_io");
    assert!(!target.exists());
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn write_failure_in_an_isolated_process_cleans_staging_and_final_target() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "publication_write_failure_child", "--nocapture"])
        .env("SAYA_PUBLICATION_LIMIT_CHILD", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn non_utf8_target_is_rejected_before_file_creation() {
    use std::os::unix::ffi::OsStringExt;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(std::ffi::OsString::from_vec(vec![0xff]));
    let result = publish(
        &path,
        b"text",
        temp.path(),
        WorkMode::Work,
        &RunCancellation::new(),
    );
    assert_eq!(result.unwrap_err().0.code, "invalid_export_path");
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
}
