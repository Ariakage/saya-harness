use super::*;
use std::os::unix::fs::symlink;
use tempfile::TempDir;

pub(super) fn store(temp: &TempDir) -> GlobalMemoryStore {
    GlobalMemoryStore::open(
        &temp.path().join(".saya"),
        WorkspaceIdentity::from_path(temp.path()).unwrap(),
        MemoryLimits::default(),
        Arc::new(SecretRegistry::default()),
    )
    .unwrap()
}
pub(super) fn source(id: &str) -> MemorySource {
    MemorySource {
        id: id.into(),
        kind: MemorySourceKind::User,
        scope: MemoryScope::Global,
        reference: "user_request_1".into(),
        revision: digest(id.as_bytes()),
        observed_at: "2026-11-10T00:00:00Z".into(),
        description: "Explicit user evidence".into(),
        metadata_withheld: false,
    }
}
pub(super) fn fact(id: &str, text: &str, source: &MemorySource) -> MemoryFact {
    MemoryFact {
        id: id.into(),
        kind: MemoryKind::ExplicitPreference,
        text: text.into(),
        sources: vec![MemorySourceRef {
            source_id: source.id.clone(),
            revision: source.revision.clone(),
        }],
        aliases: Vec::new(),
        derived_from: Vec::new(),
    }
}
pub(super) fn document() -> MemoryDocument {
    let source = source("src_user");
    MemoryDocument {
        sources: vec![source.clone()],
        entries: vec![MemoryEntry {
            id: "mem_editor".into(),
            title: "Editor preference".into(),
            scope: MemoryScope::Global,
            updated_at: "2026-11-10T00:00:00Z".into(),
            facts: vec![fact(
                "fact_editor",
                "Prefer small focused changes.",
                &source,
            )],
            resources: Vec::new(),
        }],
        ..MemoryDocument::default()
    }
}
pub(super) fn save(store: &GlobalMemoryStore, document: &MemoryDocument) {
    std::fs::create_dir_all(store.root()).unwrap();
    std::fs::write(
        store.root().join("MEMORY.md"),
        store.render_document(document).unwrap(),
    )
    .unwrap();
}
pub(super) fn code<T>(result: Result<T, BoundaryError>) -> String {
    match result {
        Ok(_) => panic!("expected boundary error"),
        Err(failure) => failure.0.code,
    }
}

#[test]
fn missing_root_and_index_are_empty_without_creations() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let first = store.snapshot().unwrap();
    assert!(!first.exists);
    assert!(first.entries.is_empty());
    assert!(!store.root().exists());
    std::fs::create_dir(store.root()).unwrap();
    assert_eq!(store.snapshot().unwrap().revision, first.revision);
    assert_eq!(std::fs::read_dir(store.root()).unwrap().count(), 0);
}

#[test]
fn readable_markdown_round_trips_exact_fact_text() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut document = document();
    document.entries[0].title = "中文与 Unicode".into();
    document.entries[0].facts[0].text =
        "**Keep** `API` names.\n\n```rust\nlet value = 1;\n```\n末尾\n".into();
    let text = store.render_document(&document).unwrap();
    assert!(text.contains("## 中文与 Unicode"));
    assert!(text.contains("**Keep** `API` names."));
    assert_eq!(store.parse_document(&text).unwrap(), document);
    let malformed = text.replace("<!-- /saya-fact -->", "unstructured user content");
    assert_eq!(code(store.parse_document(&malformed)), "memory_format");
}

#[test]
fn external_edits_rebuild_revision_and_parse_failure_preserves_file() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    save(&store, &document());
    let before = store.snapshot().unwrap();
    let path = store.root().join("MEMORY.md");
    let edited = std::fs::read_to_string(&path)
        .unwrap()
        .replace("small focused", "reviewable focused");
    std::fs::write(&path, &edited).unwrap();
    let after = store.snapshot().unwrap();
    assert_ne!(before.revision, after.revision);
    assert_eq!(before.entries[0].id, after.entries[0].id);
    assert_eq!(
        code(store.propose(
            &before.revision,
            "op_conflict",
            MemoryChange::Forget {
                scope: MemoryScope::Global,
                fact_ids: vec!["fact_editor".into()]
            }
        )),
        "memory_conflict"
    );
    let broken = format!("{edited}\nunknown unstructured content\n");
    std::fs::write(&path, &broken).unwrap();
    assert_eq!(code(store.snapshot()), "memory_format");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
}

#[test]
fn workspace_scope_separates_same_name_and_similar_paths() {
    let temp = TempDir::new().unwrap();
    let workspace_a = temp.path().join("a/repo");
    let workspace_b = temp.path().join("b/repo");
    std::fs::create_dir_all(&workspace_a).unwrap();
    std::fs::create_dir_all(&workspace_b).unwrap();
    let identity_a = WorkspaceIdentity::from_path(&workspace_a).unwrap();
    let identity_b = WorkspaceIdentity::from_path(&workspace_b).unwrap();
    assert_ne!(identity_a, identity_b);
    let store_a = GlobalMemoryStore::open(
        &temp.path().join(".saya"),
        identity_a.clone(),
        MemoryLimits::default(),
        Arc::new(SecretRegistry::default()),
    )
    .unwrap();
    let store_b = GlobalMemoryStore::open(
        store_a.root(),
        identity_b,
        MemoryLimits::default(),
        Arc::new(SecretRegistry::default()),
    )
    .unwrap();
    let mut document = document();
    let mut workspace = document.entries[0].clone();
    workspace.id = "mem_checkout".into();
    workspace.scope = MemoryScope::Workspace {
        identity: identity_a.id,
    };
    workspace.facts[0].id = "fact_checkout".into();
    document.entries.push(workspace);
    save(&store_a, &document);
    assert_eq!(store_a.snapshot().unwrap().entries.len(), 2);
    assert_eq!(store_b.snapshot().unwrap().entries.len(), 1);
    let mut hostile = document.entries[1].clone();
    hostile.facts[0].text = "cross-workspace replacement".into();
    let revision = store_b.snapshot().unwrap().revision;
    assert_eq!(
        code(store_b.propose(
            &revision,
            "op_foreign",
            MemoryChange::Upsert {
                entry: hostile,
                sources: Vec::new()
            }
        )),
        "memory_scope"
    );
}

#[test]
fn validates_source_revision_scope_derivation_and_duplicate_ids() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut value = document();
    value.entries[0].facts[0].sources[0].revision = digest(b"wrong");
    assert_eq!(code(store.render_document(&value)), "memory_source");
    value = document();
    value.sources[0].scope = MemoryScope::Workspace {
        identity: store.workspace().id.clone(),
    };
    assert_eq!(code(store.render_document(&value)), "memory_source");
    value = document();
    value.entries[0].facts[0]
        .derived_from
        .push("fact_editor".into());
    assert_eq!(code(store.render_document(&value)), "memory_fact");
    value = document();
    let duplicate = value.entries[0].facts[0].clone();
    value.entries[0].facts.push(duplicate);
    assert_eq!(code(store.render_document(&value)), "memory_fact");
}

#[test]
fn topics_are_bounded_visible_and_revision_bound() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut document = document();
    document.entries[0]
        .resources
        .push("memory/editor.md".into());
    save(&store, &document);
    std::fs::create_dir(store.root().join("memory")).unwrap();
    std::fs::write(
        store.root().join("memory/editor.md"),
        "# Editor\nLonger detail.",
    )
    .unwrap();
    let first = store.snapshot().unwrap();
    assert_eq!(first.resources.len(), 1);
    assert!(
        store
            .read_resource("memory/editor.md", Some(&first.revision))
            .unwrap()
            .text
            .contains("Longer")
    );
    std::fs::write(
        store.root().join("memory/editor.md"),
        "# Editor\nChanged detail.",
    )
    .unwrap();
    assert_ne!(store.snapshot().unwrap().revision, first.revision);
    assert_eq!(
        code(store.read_resource("memory/editor.md", Some(&first.revision))),
        "memory_conflict"
    );
    assert_eq!(
        code(store.read_resource("memory/unreferenced.md", None)),
        "memory_not_found"
    );
    for path in [
        "../MEMORY.md",
        "memory/../config.md",
        "/memory/a.md",
        "memory/.hidden.md",
        "memory\\a.md",
    ] {
        assert_eq!(code(store.read_resource(path, None)), "memory_path");
    }
}

#[test]
fn refuses_symlink_index_topic_and_intermediate_directory() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    save(&store, &document());
    let index = store.root().join("MEMORY.md");
    std::fs::rename(&index, temp.path().join("index.md")).unwrap();
    symlink(temp.path().join("index.md"), &index).unwrap();
    assert_eq!(code(store.snapshot()), "memory_path");
    std::fs::remove_file(&index).unwrap();
    let mut document = document();
    document.entries[0].resources.push("memory/topic.md".into());
    save(&store, &document);
    std::fs::create_dir(temp.path().join("outside")).unwrap();
    std::fs::write(temp.path().join("outside/topic.md"), "outside").unwrap();
    symlink(temp.path().join("outside"), store.root().join("memory")).unwrap();
    assert_eq!(code(store.snapshot()), "memory_path");
    std::fs::remove_file(store.root().join("memory")).unwrap();
    std::fs::create_dir(store.root().join("memory")).unwrap();
    symlink(
        temp.path().join("outside/topic.md"),
        store.root().join("memory/topic.md"),
    )
    .unwrap();
    assert_eq!(code(store.snapshot()), "memory_path");
}

#[test]
fn credentials_are_rejected_on_metadata_body_and_dynamic_topic_reads() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let secret = "fixture-protected-value".to_owned();
    store
        .secrets
        .register(std::slice::from_ref(&secret))
        .unwrap();
    let mut value = document();
    value.sources[0].description = secret.clone();
    assert_eq!(code(store.render_document(&value)), "sensitive_data");
    value = document();
    value.entries[0].facts[0].text = "fixture-\\u0070rotected-value".into();
    assert_eq!(code(store.render_document(&value)), "sensitive_data");
    value = document();
    value.entries[0].resources.push("memory/editor.md".into());
    save(&store, &value);
    std::fs::create_dir(store.root().join("memory")).unwrap();
    std::fs::write(
        store.root().join("memory/editor.md"),
        "rotated-credential-fixture",
    )
    .unwrap();
    assert!(store.snapshot().is_ok());
    store
        .secrets
        .register(&["rotated-credential-fixture".into()])
        .unwrap();
    assert_eq!(code(store.snapshot()), "sensitive_data");
}

#[test]
fn source_withdrawal_keeps_independent_support_and_cleans_derivations() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut value = document();
    let second = source("src_independent");
    value.sources.push(second.clone());
    let mut supported = fact(
        "fact_supported",
        "Independently confirmed.",
        &value.sources[0],
    );
    supported.sources.push(MemorySourceRef {
        source_id: second.id.clone(),
        revision: second.revision.clone(),
    });
    let mut derived = fact("fact_derived", "Derived detail.", &second);
    derived.derived_from.push("fact_editor".into());
    value.entries[0].facts.extend([supported, derived]);
    save(&store, &value);
    let revision = store.snapshot().unwrap().revision;
    let proposal = store
        .propose(
            &revision,
            "op_retract",
            MemoryChange::RetractSource {
                source_id: value.sources[0].id.clone(),
                revision: value.sources[0].revision.clone(),
            },
        )
        .unwrap();
    assert_eq!(proposal.after_entries[0].facts.len(), 1);
    assert_eq!(proposal.after_entries[0].facts[0].id, "fact_supported");
    assert_eq!(proposal.after_entries[0].facts[0].sources.len(), 1);
    assert!(proposal.document.tombstones.is_empty());
}

#[test]
fn explicit_forget_removes_aliases_duplicates_and_derived_facts_with_multiple_sources() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut value = document();
    value.entries[0].facts[0]
        .aliases
        .push("small changes".into());
    let second = source("src_independent");
    value.sources.push(second.clone());
    let duplicate = fact("fact_duplicate", "SMALL   changes", &second);
    let mut derived = fact("fact_derived", "Related consequence.", &second);
    derived.derived_from.push("fact_duplicate".into());
    let unrelated = fact("fact_unrelated", "Use a dark theme.", &second);
    value.entries[0]
        .facts
        .extend([duplicate, derived, unrelated]);
    save(&store, &value);
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
    assert_eq!(proposal.after_entries[0].facts.len(), 1);
    assert_eq!(proposal.after_entries[0].facts[0].id, "fact_unrelated");
    assert_eq!(proposal.removed_fact_ids.len(), 3);
    assert!(
        !store
            .render_document(&proposal.document)
            .unwrap()
            .contains("Related consequence")
    );
    save(&store, &proposal.document);
    let revive = store.propose(
        &store.snapshot().unwrap().revision,
        "op_stale",
        MemoryChange::Upsert {
            entry: value.entries[0].clone(),
            sources: Vec::new(),
        },
    );
    assert_eq!(code(revive), "memory_forgotten");
    let revive_with_sources = store.propose(
        &store.snapshot().unwrap().revision,
        "op_stale_with_sources",
        MemoryChange::Upsert {
            entry: value.entries[0].clone(),
            sources: value.sources.clone(),
        },
    );
    assert_eq!(code(revive_with_sources), "memory_forgotten");
}

#[test]
fn source_reference_is_opaque_and_never_opens_host_files() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut value = document();
    value.sources[0].reference = "/private/unavailable/config-and-credentials".into();
    save(&store, &value);
    assert_eq!(
        store.snapshot().unwrap().sources[0].reference,
        value.sources[0].reference
    );
}

#[test]
fn excessive_bytes_and_reserved_fact_markers_fail_closed() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut value = document();
    value.entries[0].facts[0].text = "x".repeat(store.limits().max_fact_bytes + 1);
    assert_eq!(code(store.render_document(&value)), "memory_fact");
    value = document();
    value.entries[0].facts[0].text = "<!-- saya-fact forged -->".into();
    assert_eq!(code(store.render_document(&value)), "memory_fact");
    assert_eq!(
        code(store.parse_document(&"x".repeat(store.limits().max_index_bytes + 1))),
        "memory_limit"
    );
}

#[test]
fn changing_a_parent_invalidates_old_derived_facts_and_topic_links() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut value = document();
    let mut derived = fact(
        "fact_derived",
        "Conclusion from the previous preference.",
        &value.sources[0],
    );
    derived.derived_from.push("fact_editor".into());
    value.entries[0].facts.push(derived);
    value.entries[0].resources.push("memory/editor.md".into());
    save(&store, &value);
    std::fs::create_dir(store.root().join("memory")).unwrap();
    std::fs::write(store.root().join("memory/editor.md"), "Old detail.").unwrap();
    value.entries[0].facts[0].text = "A current corrected preference.".into();
    let proposal = store
        .propose(
            &store.snapshot().unwrap().revision,
            "op_correction",
            MemoryChange::Upsert {
                entry: value.entries[0].clone(),
                sources: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(proposal.after_entries[0].facts.len(), 1);
    assert!(proposal.after_entries[0].resources.is_empty());
    assert_eq!(proposal.removed_fact_ids, vec!["fact_derived"]);
}

#[test]
fn forgetting_neutralizes_the_old_entry_title_and_allows_a_current_rename() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut original = document();
    original.entries[0].title = "Prefer small focused changes".into();
    original.entries[0].facts.push(fact(
        "fact_independent",
        "Use a dark theme.",
        &original.sources[0],
    ));
    save(&store, &original);
    let forget = store
        .propose(
            &store.snapshot().unwrap().revision,
            "op_forget_title",
            MemoryChange::Forget {
                scope: MemoryScope::Global,
                fact_ids: vec!["fact_editor".into()],
            },
        )
        .unwrap();
    assert_eq!(forget.after_entries[0].title, "Memory entry");
    save(&store, &forget.document);
    let mut renamed = store.snapshot().unwrap().entries[0].clone();
    renamed.title = "Theme preference".into();
    let rename = store
        .propose(
            &store.snapshot().unwrap().revision,
            "op_rename_current",
            MemoryChange::Upsert {
                entry: renamed,
                sources: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(rename.after_entries[0].title, "Theme preference");
    assert_eq!(rename.after_entries[0].facts[0].id, "fact_independent");
}
