use super::*;

#[test]
fn sealed_worker_protection_blocks_private_aliases_without_rescanning_private_roots() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let workspace = root.join("workspace");
    let private = root.join("private");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&private).unwrap();
    let source = private.join("MEMORY.md");
    std::fs::write(&source, "private memory").unwrap();
    std::fs::hard_link(&source, workspace.join("alias.txt")).unwrap();
    std::fs::write(workspace.join("public.txt"), "public data").unwrap();
    std::fs::hard_link(
        workspace.join("public.txt"),
        workspace.join("normal-link.txt"),
    )
    .unwrap();
    let host = ReadTools::new(&workspace, &[], false, ReadLimits::default()).unwrap();
    host.protect_paths(vec![private.clone()]).unwrap();
    let snapshot = host
        .worker_protection_snapshot("search_text", &workspace)
        .unwrap();
    assert!(!snapshot.safe_hardlinks.is_empty());
    // A restricted worker cannot inspect this path. Removing it also checks
    // historical identity protection when the surviving alias has nlink == 1.
    std::fs::remove_file(source).unwrap();
    std::fs::remove_dir(private).unwrap();
    let worker = ReadTools::new(&workspace, &[], false, ReadLimits::default()).unwrap();
    worker.install_protection_snapshot(snapshot).unwrap();
    assert_eq!(
        worker
            .read_file(Path::new("alias.txt"), 1, 10)
            .unwrap_err()
            .code,
        "path_denied"
    );
    assert_eq!(
        worker
            .read_file(Path::new("normal-link.txt"), 1, 10)
            .unwrap()
            .content,
        "public data"
    );
    let names = worker.list_dir(Path::new(".")).unwrap();
    assert!(names.items.iter().all(|item| item.name != "alias.txt"));
    let found = worker.find_files(Path::new("."), "*.txt", true).unwrap();
    assert!(
        found
            .items
            .iter()
            .all(|path| path.file_name().unwrap() != "alias.txt")
    );
    let hits = worker
        .search_text(Path::new("."), "private memory", false, true)
        .unwrap();
    assert!(hits.items.is_empty());
}

#[test]
fn unproven_or_changed_hardlinks_are_skipped_with_truncation_but_plain_files_work() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    let private = workspace.join("private");
    std::fs::create_dir(&private).unwrap();
    std::fs::write(private.join("data"), "private").unwrap();
    std::fs::write(workspace.join("public"), "original").unwrap();
    std::fs::hard_link(workspace.join("public"), workspace.join("known-link")).unwrap();
    let host = ReadTools::new(&workspace, &[], false, ReadLimits::default()).unwrap();
    host.protect_paths(vec![private]).unwrap();
    let snapshot = host
        .worker_protection_snapshot("find_files", &workspace)
        .unwrap();
    std::fs::write(workspace.join("fresh"), "ordinary").unwrap();
    std::fs::hard_link(workspace.join("fresh"), workspace.join("new-link")).unwrap();
    std::fs::write(workspace.join("public"), "changed after capture").unwrap();
    std::fs::write(workspace.join("plain"), "plain single-link file").unwrap();
    let worker = ReadTools::new(&workspace, &[], false, ReadLimits::default()).unwrap();
    worker.install_protection_snapshot(snapshot).unwrap();
    assert_eq!(
        worker
            .read_file(Path::new("new-link"), 1, 10)
            .unwrap_err()
            .code,
        "path_denied"
    );
    assert_eq!(
        worker
            .read_file(Path::new("known-link"), 1, 10)
            .unwrap_err()
            .code,
        "path_denied"
    );
    assert!(worker.read_file(Path::new("plain"), 1, 10).is_ok());
    let page = worker.find_files(Path::new("."), "*", true).unwrap();
    assert!(page.truncated);
    assert!(
        page.items
            .iter()
            .any(|path| path.file_name().unwrap() == "plain")
    );
    assert!(
        !page
            .items
            .iter()
            .any(|path| path.file_name().unwrap() == "new-link")
    );
}

#[test]
fn worker_snapshot_is_bounded_validated_and_legacy_empty_snapshots_remain_usable() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    std::fs::write(workspace.join("file"), "public").unwrap();
    let files = ReadTools::new(&workspace, &[], false, ReadLimits::default()).unwrap();
    let snapshot = ProtectedPathSnapshot {
        paths: vec![PathBuf::from("relative")],
        ..Default::default()
    };
    assert!(files.install_protection_snapshot(snapshot).is_err());
    let snapshot = ProtectedPathSnapshot {
        private_files: vec![(1, 2); 65_537],
        ..Default::default()
    };
    assert!(snapshot.validate().is_err());
    files
        .install_protection_snapshot(ProtectedPathSnapshot::default())
        .unwrap();
    assert_eq!(
        files.read_file(Path::new("file"), 1, 1).unwrap().content,
        "public"
    );
    assert!(files.protect_paths(vec![workspace.join("file")]).is_err());
}

#[test]
fn incomplete_private_identity_evidence_preserves_plain_files_and_fails_closed_for_hardlinks() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    let private = workspace.join("private");
    std::fs::create_dir(&private).unwrap();
    std::fs::write(private.join("data"), "private").unwrap();
    std::fs::write(workspace.join("plain"), "plain needle").unwrap();
    std::fs::write(workspace.join("linked"), "linked needle").unwrap();
    std::fs::hard_link(workspace.join("linked"), workspace.join("alias")).unwrap();
    let host = ReadTools::new(&workspace, &[], false, ReadLimits::default()).unwrap();
    host.protect_paths(vec![private]).unwrap();
    let mut snapshot = host
        .worker_protection_snapshot("find_files", &workspace)
        .unwrap();
    snapshot.private_scan_incomplete = true;
    snapshot.safe_hardlinks.clear();
    let worker = ReadTools::new(&workspace, &[], false, ReadLimits::default()).unwrap();
    worker.install_protection_snapshot(snapshot).unwrap();
    assert_eq!(
        worker
            .read_file(Path::new("linked"), 1, 10)
            .unwrap_err()
            .code,
        "host_protection_unavailable"
    );
    assert!(worker.read_file(Path::new("plain"), 1, 10).is_ok());
    let hits = worker
        .search_text(Path::new("."), "needle", false, true)
        .unwrap();
    assert!(hits.truncated);
    assert_eq!(hits.items.len(), 1);
    assert_eq!(hits.items[0].path.file_name().unwrap(), "plain");
}
use std::{fs, os::unix::fs::symlink};

fn fixture(bypass: bool) -> (tempfile::TempDir, ReadTools) {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".saya/memory/.saya-history")).unwrap();
    fs::create_dir_all(temp.path().join(".saya/skills/guide")).unwrap();
    fs::write(
        temp.path().join(".saya/MEMORY.md"),
        "forgotten-private-marker",
    )
    .unwrap();
    fs::write(
        temp.path().join(".saya/memory/topic.md"),
        "forgotten-private-marker",
    )
    .unwrap();
    fs::write(
        temp.path().join(".saya/memory/.saya-history/old.md"),
        "forgotten-private-marker",
    )
    .unwrap();
    fs::write(temp.path().join(".saya/AGENTS.md"), "public-project-marker").unwrap();
    fs::write(
        temp.path().join(".saya/skills/guide/SKILL.md"),
        "public-project-marker",
    )
    .unwrap();
    fs::write(temp.path().join("public.txt"), "public-project-marker").unwrap();
    let files = ReadTools::new(temp.path(), &[], bypass, ReadLimits::default()).unwrap();
    files
        .protect_paths(vec![
            temp.path().join(".saya/MEMORY.md"),
            temp.path().join(".saya/memory"),
        ])
        .unwrap();
    (temp, files)
}

#[test]
fn managed_paths_and_aliases_are_excluded_while_project_instructions_and_skills_remain_readable() {
    for bypass in [false, true] {
        let (temp, files) = fixture(bypass);
        for private in [
            ".saya/MEMORY.md",
            ".saya/memory/topic.md",
            ".saya/memory/.saya-history/old.md",
        ] {
            assert_eq!(
                files.read_file(Path::new(private), 1, 10).unwrap_err().code,
                "path_denied"
            );
        }
        assert!(files.read_file(Path::new(".saya/AGENTS.md"), 1, 10).is_ok());
        assert!(
            files
                .read_file(Path::new(".saya/skills/guide/SKILL.md"), 1, 10)
                .is_ok()
        );
        symlink(
            temp.path().join(".saya/memory"),
            temp.path().join("memory-alias"),
        )
        .unwrap();
        symlink(
            temp.path().join(".saya/MEMORY.md"),
            temp.path().join("index-alias"),
        )
        .unwrap();
        assert_eq!(
            files
                .read_file(Path::new("memory-alias/topic.md"), 1, 10)
                .unwrap_err()
                .code,
            "path_denied"
        );
        assert_eq!(
            files
                .read_file(Path::new("index-alias"), 1, 10)
                .unwrap_err()
                .code,
            "path_denied"
        );
        assert_eq!(
            files.list_dir(Path::new("memory-alias")).unwrap_err().code,
            "path_denied"
        );
        let entries = files.list_dir(Path::new(".saya")).unwrap();
        assert_eq!(
            entries
                .items
                .into_iter()
                .map(|entry| entry.name)
                .collect::<Vec<_>>(),
            vec!["AGENTS.md", "skills"]
        );
        let root = files.list_dir(Path::new(".")).unwrap();
        assert!(!root.items.iter().any(|entry| entry.name.ends_with("alias")));
    }
}

#[test]
fn recursive_find_and_search_prune_private_subtrees_even_when_ignored_files_are_included() {
    let (_temp, files) = fixture(true);
    let found = files.find_files(Path::new("."), "**/*", true).unwrap();
    assert!(found.items.iter().any(|path| path.ends_with("SKILL.md")));
    assert!(found.items.iter().any(|path| path.ends_with("public.txt")));
    assert!(!found.items.iter().any(|path| path.ends_with("MEMORY.md")
        || path.ends_with("topic.md")
        || path.ends_with("old.md")));
    assert!(
        files
            .search_text(Path::new("."), "forgotten-private-marker", false, true)
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        !files
            .search_text(Path::new("."), "public-project-marker", false, true)
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(
        files
            .find_files(Path::new(".saya/memory"), "**/*", true)
            .unwrap_err()
            .code,
        "path_denied"
    );
}

#[test]
fn protected_hardlinks_are_blocked_and_ordinary_hardlinks_remain_available() {
    let (temp, files) = fixture(true);
    fs::hard_link(
        temp.path().join(".saya/MEMORY.md"),
        temp.path().join("private-hardlink"),
    )
    .unwrap();
    fs::hard_link(
        temp.path().join("public.txt"),
        temp.path().join("ordinary-hardlink"),
    )
    .unwrap();
    assert_eq!(
        files
            .read_file(Path::new("private-hardlink"), 1, 10)
            .unwrap_err()
            .code,
        "path_denied"
    );
    assert!(
        files
            .read_file(Path::new("ordinary-hardlink"), 1, 10)
            .is_ok()
    );
    // New current memory files are discovered when an outside hardlink appears.
    fs::write(temp.path().join(".saya/memory/new.md"), "new private body").unwrap();
    fs::hard_link(
        temp.path().join(".saya/memory/new.md"),
        temp.path().join("new-hardlink"),
    )
    .unwrap();
    assert_eq!(
        files
            .read_file(Path::new("new-hardlink"), 1, 10)
            .unwrap_err()
            .code,
        "path_denied"
    );
    // Preserve known old inode identities after the managed index is replaced.
    fs::write(temp.path().join("replacement"), "new index").unwrap();
    fs::rename(
        temp.path().join("replacement"),
        temp.path().join(".saya/MEMORY.md"),
    )
    .unwrap();
    assert_eq!(
        files
            .read_file(Path::new("private-hardlink"), 1, 10)
            .unwrap_err()
            .code,
        "path_denied"
    );
    let listed = files.list_dir(Path::new(".")).unwrap();
    assert!(
        !listed
            .items
            .iter()
            .any(|entry| entry.name == "private-hardlink" || entry.name == "new-hardlink")
    );
    assert!(
        listed
            .items
            .iter()
            .any(|entry| entry.name == "ordinary-hardlink")
    );
}

#[test]
fn future_private_paths_and_failed_updates_preserve_the_existing_protection() {
    let temp = tempfile::tempdir().unwrap();
    let files = ReadTools::new(temp.path(), &[], false, ReadLimits::default()).unwrap();
    files
        .protect_paths(vec![
            temp.path().join(".saya/MEMORY.md"),
            temp.path().join(".saya/memory"),
        ])
        .unwrap();
    fs::create_dir_all(temp.path().join(".saya/memory")).unwrap();
    fs::write(temp.path().join(".saya/MEMORY.md"), "new private index").unwrap();
    fs::write(temp.path().join(".saya/memory/new.md"), "new private topic").unwrap();
    assert!(
        files
            .read_file(Path::new(".saya/MEMORY.md"), 1, 10)
            .is_err()
    );
    assert!(
        files
            .read_file(Path::new(".saya/memory/new.md"), 1, 10)
            .is_err()
    );
    let mut deep = temp.path().join("deep");
    for _ in 0..34 {
        deep.push("child");
    }
    fs::create_dir_all(&deep).unwrap();
    files.protect_paths(vec![temp.path().join("deep")]).unwrap();
    let snapshot = files
        .worker_protection_snapshot("find_files", files.workspace.as_path())
        .unwrap();
    assert!(snapshot.private_scan_incomplete);
    assert!(snapshot.safe_hardlinks.is_empty());
    fs::write(temp.path().join("plain"), "ordinary single-link file").unwrap();
    assert!(files.read_file(Path::new("plain"), 1, 10).is_ok());
    fs::hard_link(temp.path().join("plain"), temp.path().join("plain-link")).unwrap();
    assert_eq!(
        files
            .read_file(Path::new("plain-link"), 1, 10)
            .unwrap_err()
            .code,
        "host_protection_unavailable"
    );
    assert!(
        files
            .read_file(Path::new(".saya/MEMORY.md"), 1, 10)
            .is_err()
    );
    assert!(
        files
            .protect_paths(vec![PathBuf::from("relative")])
            .is_err()
    );
}

#[test]
fn fd_identity_and_case_aliases_are_checked_against_private_storage() {
    let (temp, files) = fixture(true);
    let (_, root, _) = files.resolve(&temp.path().join("public.txt")).unwrap();
    let private = root
        .dir
        .open(
            temp.path()
                .strip_prefix(&root.path)
                .unwrap()
                .join(".saya/MEMORY.md"),
        )
        .unwrap();
    assert_eq!(
        files
            .check_open_file(
                &temp.path().join("public.txt"),
                &private.metadata().unwrap()
            )
            .unwrap_err()
            .code,
        "path_denied"
    );
    let alternate = temp.path().join(".SAYA/MEMORY/TOPIC.MD");
    if alternate.exists() {
        assert_eq!(
            files.read_file(&alternate, 1, 10).unwrap_err().code,
            "path_denied"
        );
    }
}
