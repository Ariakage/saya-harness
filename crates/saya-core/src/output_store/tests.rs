use super::*;
use std::{
    fs::OpenOptions,
    io::{Seek, SeekFrom, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
};

fn limits() -> OutputLimits {
    OutputLimits {
        max_object_bytes: 256,
        max_run_bytes: 1024,
        max_store_bytes: 4096,
        max_objects: 32,
        tail_bytes: 8,
        preview_bytes: 64,
        max_page_bytes: 256,
        max_page_lines: 20,
        max_search_matches: 200,
    }
}
fn scope() -> OutputScope {
    OutputScope {
        session_id: SessionId::new(),
        run_id: RunId::new(),
        call_id: "call-fixture".into(),
        stream: OutputStream::Stdout,
    }
}
fn store(root: &tempfile::TempDir, secrets: Arc<SecretRegistry>) -> OutputStore {
    OutputStore::open(root.path().join("outputs"), limits(), secrets).unwrap()
}
fn read_options() -> ReadOptions {
    ReadOptions {
        max_bytes: 256,
        max_lines: 20,
        cursor: None,
    }
}
fn seal(store: &OutputStore, scope: OutputScope, cap: usize, text: &[u8]) -> SealedOutput {
    let mut capture = store.begin(scope, cap).unwrap();
    capture.push(text).unwrap();
    capture.seal(CaptureCompletion::Eof).unwrap()
}
fn text(page: &OutputPage) -> String {
    page.chunks
        .iter()
        .map(|chunk| chunk.text.as_str())
        .collect()
}
fn code<T>(result: Result<T, BoundaryError>) -> String {
    match result {
        Ok(_) => panic!("operation unexpectedly succeeded"),
        Err(error) => error.0.code,
    }
}

#[test]
fn sealed_objects_require_host_publication_and_keep_scope_across_reopen() {
    let root = tempfile::tempdir().unwrap();
    let secrets = Arc::new(SecretRegistry::default());
    let store = store(&root, secrets.clone());
    let source = scope();
    let mut capture = store.begin(source.clone(), 128).unwrap();
    let original = "hello\n世界\n";
    for bytes in original.as_bytes().chunks(2) {
        capture.push(bytes).unwrap();
    }
    let sealed = capture.seal(CaptureCompletion::Eof).unwrap();
    let reference = &sealed.metadata.output_ref;
    assert!(sealed.metadata.artifact_complete);
    assert_eq!(sealed.metadata.scope, source);
    assert_eq!(
        store.inventory().unwrap()[0].state,
        OutputObjectState::Sealed
    );
    assert_eq!(
        code(store.read(source.session_id, reference, read_options())),
        "output_unavailable"
    );
    store.publish(source.session_id, reference).unwrap();
    store.publish(source.session_id, reference).unwrap();
    assert_eq!(
        store.inventory().unwrap()[0].state,
        OutputObjectState::Published
    );
    assert_eq!(
        code(store.read(SessionId::new(), reference, read_options())),
        "output_denied"
    );
    let reopened = OutputStore::open(store.protected_path(), limits(), secrets).unwrap();
    let mut options = ReadOptions {
        max_bytes: 7,
        max_lines: 1,
        cursor: None,
    };
    let mut restored = String::new();
    loop {
        let page = reopened
            .read(source.session_id, reference, options.clone())
            .unwrap();
        restored.push_str(&text(&page));
        if page.next_cursor.is_none() {
            break;
        }
        options.cursor = page.next_cursor;
    }
    assert_eq!(restored, original);
}

#[test]
fn captures_keep_head_tail_gap_and_share_run_quota_between_streams() {
    let root = tempfile::tempdir().unwrap();
    let limits = OutputLimits {
        max_object_bytes: 32,
        max_run_bytes: 48,
        max_store_bytes: 128,
        tail_bytes: 4,
        ..limits()
    };
    let secrets = Arc::new(SecretRegistry::default());
    let store = OutputStore::open(root.path().join("outputs"), limits, secrets.clone()).unwrap();
    let second_store = OutputStore::open(store.protected_path(), limits, secrets).unwrap();
    let source = scope();
    let mut out = store.begin(source.clone(), 32).unwrap();
    let mut error_scope = source.clone();
    error_scope.stream = OutputStream::Stderr;
    let mut err = second_store.begin(error_scope.clone(), 32).unwrap();
    assert_eq!(err.record.capture_limit_bytes, 16);
    out.push(&[b'A'; 100]).unwrap();
    err.push(b"error tail").unwrap();
    let out = out.seal(CaptureCompletion::Eof).unwrap();
    let err = err.seal(CaptureCompletion::Eof).unwrap();
    assert_eq!(out.metadata.total_bytes_seen, 100);
    assert_eq!(out.metadata.retained_bytes, 32);
    assert_eq!(
        out.metadata.retained_ranges,
        vec![
            OutputRange { start: 0, end: 28 },
            OutputRange {
                start: 96,
                end: 100
            }
        ]
    );
    assert_eq!(out.metadata.gaps, vec![OutputRange { start: 28, end: 96 }]);
    assert!(!out.metadata.artifact_complete);
    assert_eq!(err.metadata.scope.stream, OutputStream::Stderr);
    assert!(err.metadata.artifact_complete);
    let later = second_store.begin(source, 32).unwrap();
    assert_eq!(later.record.capture_limit_bytes, 6);
}

#[test]
fn raw_credentials_split_across_chunks_and_capture_edges_never_reach_disk() {
    let root = tempfile::tempdir().unwrap();
    let secrets = Arc::new(SecretRegistry::default());
    let secret = "fixture-secret-value";
    secrets.register(&[secret.into()]).unwrap();
    let store = store(&root, secrets);
    let source = scope();
    let mut capture = store.begin(source.clone(), 32).unwrap();
    let original = format!(
        "{}{}{}",
        "A".repeat(20),
        secret,
        "Z".repeat(2 * 1024 * 1024)
    );
    capture.push(&original.as_bytes()[..25]).unwrap();
    for chunk in original.as_bytes()[25..].chunks(4096) {
        capture.push(chunk).unwrap();
    }
    let sealed = capture.seal(CaptureCompletion::Eof).unwrap();
    store
        .publish(source.session_id, &sealed.metadata.output_ref)
        .unwrap();
    let page = store
        .read(
            source.session_id,
            &sealed.metadata.output_ref,
            read_options(),
        )
        .unwrap();
    assert!(text(&page).starts_with(&"A".repeat(20)));
    assert!(text(&page).ends_with(&"Z".repeat(8)));
    assert!(!text(&page).contains("fixture"));
    assert!(!page.metadata.artifact_complete);
    for path in std::fs::read_dir(store.protected_path()).unwrap() {
        let bytes = std::fs::read(path.unwrap().path()).unwrap();
        assert!(
            !bytes
                .windows(secret.len())
                .any(|window| window == secret.as_bytes())
        );
    }
    let mut safe = store.begin(source.clone(), 256).unwrap();
    safe.push(format!("before {secret} after").as_bytes())
        .unwrap();
    let safe = safe.seal(CaptureCompletion::Eof).unwrap();
    store
        .publish(source.session_id, &safe.metadata.output_ref)
        .unwrap();
    assert!(
        text(
            &store
                .read(source.session_id, &safe.metadata.output_ref, read_options())
                .unwrap()
        )
        .contains("[REDACTED]")
    );
}

#[test]
fn complete_encoded_values_redact_and_unsafe_truncated_or_binary_windows_withhold() {
    let root = tempfile::tempdir().unwrap();
    let secrets = Arc::new(SecretRegistry::default());
    secrets.register(&["fixture-secret".into()]).unwrap();
    let store = store(&root, secrets);
    let source = scope();
    let complete = seal(
        &store,
        source.clone(),
        256,
        br#"{"token":"fixture-\u0073ecret","safe":1}"#,
    );
    store
        .publish(source.session_id, &complete.metadata.output_ref)
        .unwrap();
    assert!(
        text(
            &store
                .read(
                    source.session_id,
                    &complete.metadata.output_ref,
                    read_options()
                )
                .unwrap()
        )
        .contains("[REDACTED]")
    );
    let encoded = seal(
        &store,
        source.clone(),
        16,
        br#"prefix {"token":"fixture-\u0073ecret"} suffix"#,
    );
    assert_eq!(encoded.metadata.coverage, OutputCoverage::Withheld);
    assert_eq!(
        encoded.metadata.withheld_reason,
        Some(WithheldReason::EncodedBoundary)
    );
    assert!(encoded.metadata.preview_head.is_empty());
    let binary = seal(&store, source, 64, b"text\xffsecret");
    assert_eq!(
        binary.metadata.withheld_reason,
        Some(WithheldReason::NonUtf8)
    );
    assert!(!binary.metadata.artifact_complete);
}

#[test]
fn dynamic_registry_redacts_previews_and_invalidates_old_projection_cursors() {
    let root = tempfile::tempdir().unwrap();
    let secrets = Arc::new(SecretRegistry::default());
    let store = store(&root, secrets.clone());
    let source = scope();
    let secret = "rotated-fixture-credential";
    let sealed = seal(
        &store,
        source.clone(),
        256,
        format!("first\n{secret}\nlast\n").as_bytes(),
    );
    store
        .publish(source.session_id, &sealed.metadata.output_ref)
        .unwrap();
    let page = store
        .read(
            source.session_id,
            &sealed.metadata.output_ref,
            ReadOptions {
                max_bytes: 6,
                max_lines: 1,
                cursor: None,
            },
        )
        .unwrap();
    let old_cursor = page.next_cursor.unwrap();
    secrets.register(&[secret.into()]).unwrap();
    let current = store
        .read(
            source.session_id,
            &sealed.metadata.output_ref,
            read_options(),
        )
        .unwrap();
    assert!(!text(&current).contains(secret));
    assert!(!current.metadata.preview_head.contains(secret));
    assert!(!current.metadata.preview_tail.contains(secret));
    assert_eq!(
        code(store.read(
            source.session_id,
            &sealed.metadata.output_ref,
            ReadOptions {
                cursor: Some(old_cursor),
                ..read_options()
            }
        )),
        "cursor_invalid"
    );
}

#[test]
fn search_pages_include_bounded_context_and_direct_read_cursors() {
    let root = tempfile::tempdir().unwrap();
    let store = store(&root, Arc::new(SecretRegistry::default()));
    let source = scope();
    let sealed = seal(
        &store,
        source.clone(),
        256,
        "α\nneedle one\nordinary\nneedle two\n".as_bytes(),
    );
    store
        .publish(source.session_id, &sealed.metadata.output_ref)
        .unwrap();
    let options = SearchOptions {
        pattern: "needle".into(),
        max_matches: 1,
        max_bytes: 128,
        context_lines: 1,
        ..SearchOptions::default()
    };
    let first = store
        .search(
            source.session_id,
            &sealed.metadata.output_ref,
            options.clone(),
        )
        .unwrap();
    assert_eq!(first.matches.len(), 1);
    assert!(first.matches[0].text.contains("needle one"));
    assert!(first.matches[0].text.contains("ordinary"));
    let page = store
        .read(
            source.session_id,
            &sealed.metadata.output_ref,
            ReadOptions {
                cursor: Some(first.matches[0].cursor.clone()),
                ..read_options()
            },
        )
        .unwrap();
    assert!(text(&page).contains("needle one"));
    let second = store
        .search(
            source.session_id,
            &sealed.metadata.output_ref,
            SearchOptions {
                cursor: first.next_cursor,
                ..options
            },
        )
        .unwrap();
    assert_eq!(second.matches.len(), 1);
    assert!(second.matches[0].text.contains("needle two"));
    let regex = store
        .search(
            source.session_id,
            &sealed.metadata.output_ref,
            SearchOptions {
                pattern: "^needle".into(),
                regex: true,
                max_bytes: 128,
                ..SearchOptions::default()
            },
        )
        .unwrap();
    assert_eq!(regex.matches.len(), 2);
    assert_eq!(
        code(store.search(
            source.session_id,
            &sealed.metadata.output_ref,
            SearchOptions {
                pattern: "[".into(),
                regex: true,
                max_bytes: 128,
                ..SearchOptions::default()
            }
        )),
        "invalid_arguments"
    );
    assert_eq!(
        code(store.read(
            source.session_id,
            &sealed.metadata.output_ref,
            ReadOptions {
                max_bytes: 1,
                ..read_options()
            }
        )),
        "output_query_limit"
    );
    let other = seal(&store, source.clone(), 32, b"other text");
    store
        .publish(source.session_id, &other.metadata.output_ref)
        .unwrap();
    assert_eq!(
        code(store.read(
            source.session_id,
            &other.metadata.output_ref,
            ReadOptions {
                cursor: Some(first.matches[0].cursor.clone()),
                ..read_options()
            }
        )),
        "cursor_invalid"
    );
}

#[test]
fn search_context_handles_midline_unicode_cursors_and_empty_regex_matches() {
    let root = tempfile::tempdir().unwrap();
    let store = store(&root, Arc::new(SecretRegistry::default()));
    let source = scope();
    let output = seal(&store, source.clone(), 128, "界 match\n".as_bytes());
    store
        .publish(source.session_id, &output.metadata.output_ref)
        .unwrap();
    let first = store
        .read(
            source.session_id,
            &output.metadata.output_ref,
            ReadOptions {
                max_bytes: 3,
                max_lines: 1,
                cursor: None,
            },
        )
        .unwrap();
    let matches = store
        .search(
            source.session_id,
            &output.metadata.output_ref,
            SearchOptions {
                pattern: "match".into(),
                cursor: first.next_cursor,
                context_lines: 1,
                max_bytes: 128,
                ..SearchOptions::default()
            },
        )
        .unwrap();
    assert_eq!(matches.matches.len(), 1);
    assert!(matches.matches[0].text.contains("界 match"));
    let ending = seal(&store, source.clone(), 128, "界".as_bytes());
    store
        .publish(source.session_id, &ending.metadata.output_ref)
        .unwrap();
    let matches = store
        .search(
            source.session_id,
            &ending.metadata.output_ref,
            SearchOptions {
                pattern: "$".into(),
                regex: true,
                max_bytes: 3,
                context_lines: 0,
                ..SearchOptions::default()
            },
        )
        .unwrap();
    assert_eq!(matches.matches[0].text, "界");
}

#[test]
fn raw_capture_memory_and_stored_text_remain_bounded_for_large_producers() {
    let root = tempfile::tempdir().unwrap();
    let store = store(&root, Arc::new(SecretRegistry::default()));
    let source = scope();
    let mut capture = store.begin(source, 64).unwrap();
    for _ in 0..256 {
        capture.push(&[b'x'; 8192]).unwrap();
    }
    assert_eq!(capture.total_bytes_seen(), 2 * 1024 * 1024);
    assert!(capture.head.len() <= 64 + BOUNDARY_GUARD);
    assert!(capture.tail.len() <= 8 + BOUNDARY_GUARD);
    let output = capture.seal(CaptureCompletion::Eof).unwrap();
    assert_eq!(output.metadata.retained_bytes, 64);
    assert!(!output.metadata.artifact_complete);
}

#[test]
fn leases_pins_pending_reservations_and_orphan_temporary_objects_control_gc() {
    let root = tempfile::tempdir().unwrap();
    let secrets = Arc::new(SecretRegistry::default());
    let store = store(&root, secrets.clone());
    let other = OutputStore::open(store.protected_path(), limits(), secrets).unwrap();
    let source = scope();
    let pending = store.begin(source.clone(), 64).unwrap();
    assert_eq!(
        other.collect_garbage(&BTreeSet::new(), 32).unwrap().leased,
        1
    );
    drop(pending);
    assert_eq!(
        other.collect_garbage(&BTreeSet::new(), 32).unwrap().removed,
        1
    );
    let sealed = seal(&store, source.clone(), 64, b"retained");
    store
        .publish(source.session_id, &sealed.metadata.output_ref)
        .unwrap();
    assert_eq!(
        other.collect_garbage(&BTreeSet::new(), 32).unwrap().leased,
        1
    );
    let reference = sealed.metadata.output_ref.clone();
    drop(sealed);
    let lease = store.lease(source.session_id, &reference).unwrap();
    assert_eq!(
        other.collect_garbage(&BTreeSet::new(), 32).unwrap().leased,
        1
    );
    drop(lease);
    assert_eq!(
        other
            .collect_garbage(&BTreeSet::from([reference.clone()]), 32)
            .unwrap()
            .pinned,
        1
    );
    let orphan = store
        .protected_path()
        .join(format!(".tmp-{}", uuid::Uuid::now_v7().simple()));
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(orphan)
        .unwrap()
        .write_all(b"safe orphan")
        .unwrap();
    assert_eq!(
        other.collect_garbage(&BTreeSet::new(), 32).unwrap().removed,
        2
    );
    assert!(other.inventory().unwrap().is_empty());
    assert_eq!(
        code(other.describe(source.session_id, &reference)),
        "output_unavailable"
    );
}

#[test]
fn object_digest_permissions_links_and_missing_objects_fail_closed() {
    let root = tempfile::tempdir().unwrap();
    let store = store(&root, Arc::new(SecretRegistry::default()));
    let source = scope();
    let sealed = seal(&store, source.clone(), 64, b"visible text");
    store
        .publish(source.session_id, &sealed.metadata.output_ref)
        .unwrap();
    let reference = sealed.metadata.output_ref.clone();
    let path = store
        .protected_path()
        .join(format!("{}.out", &reference[..36]));
    let mut file = OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::End(-1)).unwrap();
    file.write_all(b"X").unwrap();
    assert_eq!(
        code(store.describe(source.session_id, &reference)),
        "output_corrupt"
    );
    drop(file);
    drop(sealed);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        code(store.describe(source.session_id, &reference)),
        "output_unavailable"
    );
    let target = root.path().join("outside");
    std::fs::write(&target, b"preserve outside").unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert_eq!(
        code(store.describe(source.session_id, &reference)),
        "output_unavailable"
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"preserve outside");
    let linked = root.path().join("linked-root");
    std::os::unix::fs::symlink(store.protected_path(), &linked).unwrap();
    assert_eq!(
        code(OutputStore::open(
            linked,
            limits(),
            Arc::new(SecretRegistry::default())
        )),
        "output_permissions"
    );
    let permissive = root.path().join("public-root");
    std::fs::create_dir(&permissive).unwrap();
    std::fs::set_permissions(&permissive, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        code(OutputStore::open(
            permissive,
            limits(),
            Arc::new(SecretRegistry::default())
        )),
        "output_permissions"
    );
}

#[test]
fn capture_completion_is_separate_from_parent_execution_and_queries_cancel() {
    let root = tempfile::tempdir().unwrap();
    let store = store(&root, Arc::new(SecretRegistry::default()));
    let source = scope();
    for completion in [CaptureCompletion::Cancelled, CaptureCompletion::ReadFailed] {
        let mut capture = store.begin(source.clone(), 64).unwrap();
        capture.push(b"partial evidence").unwrap();
        let sealed = capture.seal(completion).unwrap();
        assert!(!sealed.metadata.artifact_complete);
        assert_eq!(sealed.metadata.completion, completion);
    }
    let cancel = AtomicBool::new(true);
    assert_eq!(
        code(store.read_cancellable(
            source.session_id,
            "not-a-reference",
            read_options(),
            &cancel
        )),
        "cancelled"
    );
    assert_eq!(
        code(store.search_cancellable(
            source.session_id,
            "not-a-reference",
            SearchOptions::default(),
            &cancel
        )),
        "cancelled"
    );
}

#[test]
fn failed_atomic_install_never_overwrites_an_existing_destination_or_leaks_raw_text() {
    let root = tempfile::tempdir().unwrap();
    let secrets = Arc::new(SecretRegistry::default());
    secrets.register(&["fixture-secret".into()]).unwrap();
    let store = store(&root, secrets);
    let mut capture = store.begin(scope(), 64).unwrap();
    capture.push(b"fixture-secret").unwrap();
    let target = root.path().join("outside");
    std::fs::write(&target, b"preserve outside").unwrap();
    std::os::unix::fs::symlink(
        &target,
        store
            .protected_path()
            .join(format!("{}.out", capture.reservation_id())),
    )
    .unwrap();
    assert_eq!(code(capture.seal(CaptureCompletion::Eof)), "output_io");
    assert_eq!(std::fs::read(&target).unwrap(), b"preserve outside");
    for entry in std::fs::read_dir(store.protected_path()).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_symlink() {
            continue;
        }
        let data = std::fs::read(entry.path()).unwrap();
        assert!(!data.windows(14).any(|window| window == b"fixture-secret"));
    }
}

#[test]
fn zero_capture_limits_still_count_observed_bytes_without_writing_body() {
    let root = tempfile::tempdir().unwrap();
    let store = store(&root, Arc::new(SecretRegistry::default()));
    let output = seal(&store, scope(), 0, b"observed but not retained");
    assert_eq!(output.metadata.total_bytes_seen, 25);
    assert_eq!(output.metadata.retained_bytes, 0);
    assert_eq!(
        output.metadata.withheld_reason,
        Some(WithheldReason::CaptureDisabled)
    );
}

#[test]
fn reopening_repairs_only_recognized_interrupted_install_links() {
    let root = tempfile::tempdir().unwrap();
    let secrets = Arc::new(SecretRegistry::default());
    let store = store(&root, secrets.clone());
    let source = scope();
    let sealed = seal(&store, source.clone(), 64, b"recoverable output");
    let reference = sealed.metadata.output_ref.clone();
    let object = store
        .protected_path()
        .join(format!("{}.out", &reference[..36]));
    drop(sealed);
    let interrupted_temporary = store
        .protected_path()
        .join(format!(".tmp-{}", uuid::Uuid::now_v7().simple()));
    std::fs::hard_link(&object, &interrupted_temporary).unwrap();
    let reopened = OutputStore::open(store.protected_path(), limits(), secrets.clone()).unwrap();
    assert!(!interrupted_temporary.exists());
    reopened.publish(source.session_id, &reference).unwrap();
    assert_eq!(
        text(
            &reopened
                .read(source.session_id, &reference, read_options())
                .unwrap()
        ),
        "recoverable output"
    );
    std::fs::hard_link(&object, root.path().join("external-link")).unwrap();
    assert_eq!(
        code(OutputStore::open(store.protected_path(), limits(), secrets)),
        "output_permissions"
    );
}

#[test]
fn gc_can_make_bounded_progress_through_an_overquota_orphan_backlog() {
    let root = tempfile::tempdir().unwrap();
    let configured = OutputLimits {
        max_objects: 1,
        ..limits()
    };
    let store = OutputStore::open(
        root.path().join("outputs"),
        configured,
        Arc::new(SecretRegistry::default()),
    )
    .unwrap();
    for _ in 0..25 {
        let path = store
            .protected_path()
            .join(format!(".tmp-{}", uuid::Uuid::now_v7().simple()));
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap()
            .write_all(b"safe")
            .unwrap();
    }
    assert_eq!(code(store.begin(scope(), 16)), "output_quota");
    for _ in 0..25 {
        assert_eq!(
            store.collect_garbage(&BTreeSet::new(), 1).unwrap().removed,
            1
        );
    }
    assert!(store.inventory().unwrap().is_empty());
    assert!(store.begin(scope(), 16).is_ok());
}

#[test]
fn gc_prefers_unpublished_objects_over_published_history() {
    let root = tempfile::tempdir().unwrap();
    let store = store(&root, Arc::new(SecretRegistry::default()));
    let source = scope();
    let published = seal(&store, source.clone(), 64, b"older published history");
    let published_ref = published.metadata.output_ref.clone();
    store.publish(source.session_id, &published_ref).unwrap();
    drop(published);
    let orphan = seal(&store, source.clone(), 64, b"newer unassociated capture");
    let orphan_ref = orphan.metadata.output_ref.clone();
    drop(orphan);
    assert_eq!(
        store.collect_garbage(&BTreeSet::new(), 1).unwrap().removed,
        1
    );
    let inventory = store.inventory().unwrap();
    assert_eq!(inventory.len(), 1);
    assert_eq!(
        inventory[0].output_ref.as_deref(),
        Some(published_ref.as_str())
    );
    assert!(
        !inventory
            .iter()
            .any(|item| item.output_ref.as_deref() == Some(orphan_ref.as_str()))
    );
    assert!(
        store
            .describe(source.session_id, &published_ref)
            .unwrap()
            .artifact_complete
    );
}

#[test]
fn regex_limits_are_explicit_while_literal_search_still_finds_long_lines() {
    let root = tempfile::tempdir().unwrap();
    let store = OutputStore::open(
        root.path().join("outputs"),
        OutputLimits::default(),
        Arc::new(SecretRegistry::default()),
    )
    .unwrap();
    let source = scope();
    let original = format!("{}needle", "x".repeat(70 * 1024));
    let output = seal(&store, source.clone(), 128 * 1024, original.as_bytes());
    store
        .publish(source.session_id, &output.metadata.output_ref)
        .unwrap();
    assert_eq!(
        code(store.search(
            source.session_id,
            &output.metadata.output_ref,
            SearchOptions {
                pattern: "needle".into(),
                regex: true,
                ..SearchOptions::default()
            }
        )),
        "output_query_limit"
    );
    let literal = store
        .search(
            source.session_id,
            &output.metadata.output_ref,
            SearchOptions {
                pattern: "needle".into(),
                ..SearchOptions::default()
            },
        )
        .unwrap();
    assert!(literal.matches[0].text.contains("needle"));
    assert!(literal.matches[0].truncated);
    assert!(
        serde_json::from_value::<OutputCursor>(serde_json::json!({
            "segment":0,"byte_offset":0,"view_sha256":"a".repeat(64),"extra":true
        }))
        .is_err()
    );
}
