use super::projection::{
    MEMORY_CONTEXT_INSTRUCTIONS, MemoryProjectionOptions, MemoryProjectionStatus,
};
use super::query::{MemoryMatchField, MemoryQueryLimits, MemoryReadOptions, MemorySearchOptions};
use super::tests::{code, document, fact, save, store};
use super::*;
use tempfile::TempDir;

#[test]
fn fact_pages_preserve_original_unicode_bytes_and_all_provenance() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut original = document();
    original.entries[0].facts[0].text = "中文\\\nUnicode é🙂\n".repeat(200);
    save(&store, &original);
    let limits = MemoryQueryLimits {
        max_page_bytes: 1600,
        ..MemoryQueryLimits::default()
    };
    let mut cursor = None;
    let mut collected = String::new();
    let mut count = 0;
    loop {
        let page = store
            .read_page(
                MemoryReadOptions {
                    cursor,
                    ..MemoryReadOptions::default()
                },
                limits,
            )
            .unwrap();
        assert!(serde_json::to_vec(&page).unwrap().len() <= limits.max_page_bytes);
        for fragment in &page.items {
            assert_eq!(fragment.reference.entry_id, "mem_editor");
            assert_eq!(fragment.reference.fact_id, "fact_editor");
            assert_eq!(
                fragment.reference.sources,
                original.entries[0].facts[0].sources
            );
            assert_eq!(fragment.start_byte, collected.len());
            assert_eq!(fragment.end_byte - fragment.start_byte, fragment.text.len());
            assert!(valid_digest(&fragment.reference.fact_revision));
            collected.push_str(&fragment.text);
        }
        cursor = page.next_cursor;
        count += 1;
        assert!(count < 100);
        if cursor.is_none() {
            break;
        }
    }
    assert!(count > 1);
    assert_eq!(collected, original.entries[0].facts[0].text);
}

#[test]
fn cursors_bind_filters_host_store_and_original_utf8_boundaries() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut original = document();
    original.entries[0].facts[0].text = "中文🙂".repeat(400);
    save(&store, &original);
    let limits = MemoryQueryLimits {
        max_page_bytes: 1600,
        ..MemoryQueryLimits::default()
    };
    let first = store
        .read_page(MemoryReadOptions::default(), limits)
        .unwrap();
    let cursor = first.next_cursor.unwrap();
    let other = GlobalMemoryStore::open(
        &temp.path().join("other-memory"),
        store.workspace().clone(),
        store.limits(),
        Arc::new(SecretRegistry::default()),
    )
    .unwrap();
    save(&other, &original);
    assert_eq!(
        store.snapshot().unwrap().revision,
        other.snapshot().unwrap().revision
    );
    assert_eq!(
        code(other.read_page(
            MemoryReadOptions {
                cursor: Some(cursor.clone()),
                ..MemoryReadOptions::default()
            },
            limits
        )),
        "memory_cursor_invalid"
    );
    assert_eq!(
        code(store.read_page(
            MemoryReadOptions {
                entry_id: Some("mem_unknown".into()),
                cursor: Some(cursor.clone()),
                ..MemoryReadOptions::default()
            },
            limits
        )),
        "memory_cursor_invalid"
    );
    let mut broken = cursor.clone();
    broken.byte_offset = 1;
    assert_eq!(
        code(store.read_page(
            MemoryReadOptions {
                cursor: Some(broken),
                ..MemoryReadOptions::default()
            },
            limits
        )),
        "memory_cursor_invalid"
    );
    let mut broken = cursor;
    broken.record_index = usize::MAX;
    assert_eq!(
        code(store.read_page(
            MemoryReadOptions {
                cursor: Some(broken),
                ..MemoryReadOptions::default()
            },
            limits
        )),
        "memory_cursor_invalid"
    );
}

#[test]
fn an_old_cursor_is_explicitly_stale_after_forgetting_its_read_target() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut original = document();
    original.entries[0].facts[0].text = "Historical private preference. ".repeat(100);
    original.entries[0].facts.push(fact(
        "fact_current",
        "Independent preference.",
        &original.sources[0],
    ));
    save(&store, &original);
    let limits = MemoryQueryLimits {
        max_page_bytes: 1600,
        ..MemoryQueryLimits::default()
    };
    let mut options = MemoryReadOptions {
        fact_id: Some("fact_editor".into()),
        ..MemoryReadOptions::default()
    };
    options.cursor = store
        .read_page(options.clone(), limits)
        .unwrap()
        .next_cursor;
    assert!(options.cursor.is_some());
    let forget = store
        .propose(
            &store.snapshot().unwrap().revision,
            "op_forget_cursor",
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
    assert_eq!(
        code(store.read_page(options, limits)),
        "memory_cursor_stale"
    );
}

#[test]
fn unicode_normalized_aliases_and_literal_queries_find_current_facts() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut original = document();
    original.entries[0].facts[0].text = "Use a dark interface.".into();
    original.entries[0].facts[0].aliases = vec!["視覚設定".into(), "theme_alias".into()];
    original.entries[0].facts.push(fact(
        "fact_unicode",
        "The café module keeps Rust identifiers.",
        &original.sources[0],
    ));
    save(&store, &original);
    let alias = store
        .search(
            MemorySearchOptions {
                query: "視覚設定".into(),
                ..MemorySearchOptions::default()
            },
            MemoryQueryLimits::default(),
        )
        .unwrap();
    assert_eq!(alias.matches[0].fact.reference.fact_id, "fact_editor");
    assert!(
        alias.matches[0]
            .matched_fields
            .contains(&MemoryMatchField::Alias)
    );
    assert_eq!(alias.matches[0].matched_aliases, vec!["視覚設定"]);
    let unicode = store
        .search(
            MemorySearchOptions {
                query: "CAFE\u{301}".into(),
                ..MemorySearchOptions::default()
            },
            MemoryQueryLimits::default(),
        )
        .unwrap();
    assert_eq!(unicode.matches[0].fact.reference.fact_id, "fact_unicode");
    assert!(
        unicode.matches[0]
            .matched_fields
            .contains(&MemoryMatchField::Text)
    );
    let literal = store
        .search(
            MemorySearchOptions {
                query: ".*not-a-regex.*".into(),
                ..MemorySearchOptions::default()
            },
            MemoryQueryLimits::default(),
        )
        .unwrap();
    assert!(literal.matches.is_empty());
}

#[test]
fn search_paging_is_bounded_and_query_changes_cannot_reuse_a_cursor() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut original = document();
    original.entries[0].facts[0].text = "Rust style preference.".into();
    original.entries[0].facts.push(fact(
        "fact_second",
        "Rust naming preference.",
        &original.sources[0],
    ));
    save(&store, &original);
    let limits = MemoryQueryLimits::default();
    let first = store
        .search(
            MemorySearchOptions {
                query: "Rust".into(),
                max_items: 1,
                ..MemorySearchOptions::default()
            },
            limits,
        )
        .unwrap();
    assert!(serde_json::to_vec(&first).unwrap().len() <= limits.max_page_bytes);
    let cursor = first.next_cursor.unwrap();
    let second = store
        .search(
            MemorySearchOptions {
                query: " RUST ".into(),
                cursor: Some(cursor.clone()),
                ..MemorySearchOptions::default()
            },
            limits,
        )
        .unwrap();
    assert_eq!(second.matches.len(), 1);
    assert_ne!(
        second.matches[0].fact.reference.fact_id,
        first.matches[0].fact.reference.fact_id
    );
    assert_eq!(
        code(store.search(
            MemorySearchOptions {
                query: "theme".into(),
                cursor: Some(cursor),
                ..MemorySearchOptions::default()
            },
            limits
        )),
        "memory_cursor_invalid"
    );
}

#[test]
fn search_returns_a_relevant_original_text_excerpt_and_reports_partial_evidence() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut original = document();
    original.entries[0].facts[0].text = format!(
        "{}\nSpecificError42 occurred after the build.\n{}",
        "unrelated material\n".repeat(200),
        "later context\n".repeat(200)
    );
    save(&store, &original);
    let page = store
        .search(
            MemorySearchOptions {
                query: "SpecificError42".into(),
                max_bytes: 1800,
                ..MemorySearchOptions::default()
            },
            MemoryQueryLimits::default(),
        )
        .unwrap();
    assert_eq!(page.matches.len(), 1);
    let fragment = &page.matches[0].fact;
    assert!(fragment.start_byte > 0);
    assert!(fragment.text.contains("SpecificError42"));
    assert_eq!(
        fragment.text,
        original.entries[0].facts[0].text[fragment.start_byte..fragment.end_byte]
    );
    assert!(page.truncated);
    assert!(page.next_cursor.is_none());
}

#[test]
fn routing_only_projection_contains_no_fact_body_or_implicit_source_reads() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut original = document();
    original.entries[0].facts[0].text =
        "A detailed historical preference only for explicit reading.".into();
    original.sources[0].reference = "/unavailable/arbitrary-host-path".into();
    save(&store, &original);
    let projection = store
        .project(
            MemoryProjectionOptions {
                body_bytes: 0,
                ..MemoryProjectionOptions::default()
            },
            MemoryQueryLimits::default(),
        )
        .unwrap();
    assert_eq!(projection.status, MemoryProjectionStatus::Included);
    assert_eq!(projection.routing.len(), 1);
    assert!(projection.selected.is_empty());
    assert!(!projection.text.contains(&original.entries[0].facts[0].text));
    assert!(!projection.text.contains(&original.sources[0].reference));
}

#[test]
fn projection_filters_foreign_workspaces_and_selects_relevant_facts_first() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut original = document();
    let mut current = original.entries[0].clone();
    current.id = "mem_workspace".into();
    current.title = "Rust workspace".into();
    current.scope = MemoryScope::Workspace {
        identity: store.workspace().id.clone(),
    };
    current.facts[0] = fact(
        "fact_workspace",
        "SpecificError42 identifies the current Rust task.",
        &original.sources[0],
    );
    let mut foreign = current.clone();
    foreign.id = "mem_foreign".into();
    foreign.scope = MemoryScope::Workspace {
        identity: format!("ws_{}", digest(b"other checkout")),
    };
    foreign.facts[0] = fact(
        "fact_foreign",
        "SpecificError42 belongs to another private checkout.",
        &original.sources[0],
    );
    original.entries.extend([current, foreign]);
    save(&store, &original);
    let projection = store
        .project(
            MemoryProjectionOptions {
                query: "SpecificError42".into(),
                max_selected_facts: 1,
                ..MemoryProjectionOptions::default()
            },
            MemoryQueryLimits::default(),
        )
        .unwrap();
    assert_eq!(projection.selected[0].reference.fact_id, "fact_workspace");
    assert!(projection.text.contains("fact_workspace"));
    assert!(!projection.text.contains("fact_foreign"));
    assert!(!projection.text.contains("another private checkout"));
}

#[test]
fn current_edits_and_forgetting_change_projection_keys_without_cached_old_facts() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut original = document();
    original.entries[0].facts.push(fact(
        "fact_independent",
        "A current independent preference.",
        &original.sources[0],
    ));
    save(&store, &original);
    let first = store
        .project(
            MemoryProjectionOptions::default(),
            MemoryQueryLimits::default(),
        )
        .unwrap();
    assert_eq!(
        store
            .project(
                MemoryProjectionOptions::default(),
                MemoryQueryLimits::default()
            )
            .unwrap()
            .cache_key,
        first.cache_key
    );
    original.entries[0].facts[0].text = "An externally edited preference.".into();
    save(&store, &original);
    let second = store
        .project(
            MemoryProjectionOptions::default(),
            MemoryQueryLimits::default(),
        )
        .unwrap();
    assert_ne!(second.cache_key, first.cache_key);
    let forget = store
        .propose(
            &store.snapshot().unwrap().revision,
            "op_forget_projection",
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
    let current = store
        .project(
            MemoryProjectionOptions::default(),
            MemoryQueryLimits::default(),
        )
        .unwrap();
    assert_ne!(current.cache_key, second.cache_key);
    assert_ne!(current.forget_revision, second.forget_revision);
    assert!(!current.text.contains("externally edited preference"));
    assert!(current.selected[0].reference.source_metadata_withheld);
}

#[test]
fn projection_uses_actual_serialized_bounds_and_handles_zero_remaining_budget() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let empty = store
        .project(
            MemoryProjectionOptions::default(),
            MemoryQueryLimits::default(),
        )
        .unwrap();
    assert_eq!(empty.status, MemoryProjectionStatus::Empty);
    assert!(empty.text.is_empty());
    assert!(!store.root().exists());
    let mut original = document();
    original.entries[0].facts[0].text = "中\\\"Unicode\n".repeat(800);
    save(&store, &original);
    for max_bytes in [0, 1, 512, 1800, 4096] {
        let projection = store
            .project(
                MemoryProjectionOptions {
                    max_bytes,
                    ..MemoryProjectionOptions::default()
                },
                MemoryQueryLimits::default(),
            )
            .unwrap();
        assert!(projection.text.len() <= max_bytes);
        if max_bytes <= 512 {
            assert_eq!(projection.status, MemoryProjectionStatus::BudgetExhausted);
        }
    }
    let body_only = store
        .project(
            MemoryProjectionOptions {
                routing_bytes: 0,
                max_bytes: 4096,
                ..MemoryProjectionOptions::default()
            },
            MemoryQueryLimits::default(),
        )
        .unwrap();
    assert!(!body_only.selected.is_empty());
    assert!(!body_only.selected[0].complete);
    assert!(body_only.truncated);
}

#[test]
fn mandatory_metadata_never_loses_source_attribution_to_fit_a_tiny_page() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    save(&store, &document());
    assert_eq!(
        code(store.read_page(
            MemoryReadOptions {
                max_bytes: 512,
                ..MemoryReadOptions::default()
            },
            MemoryQueryLimits::default()
        )),
        "memory_query_limit"
    );
    let page = store
        .read_page(MemoryReadOptions::default(), MemoryQueryLimits::default())
        .unwrap();
    assert_eq!(page.items[0].reference.sources.len(), 1);
    assert_eq!(page.items[0].reference.sources[0].source_id, "src_user");
}

#[test]
fn query_and_projection_cancel_before_reading_and_reject_current_credentials() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let cancel = AtomicBool::new(true);
    assert_eq!(
        code(store.read_page_cancellable(
            MemoryReadOptions::default(),
            MemoryQueryLimits::default(),
            &cancel
        )),
        "cancelled"
    );
    assert_eq!(
        code(store.search_cancellable(
            MemorySearchOptions {
                query: "test".into(),
                ..MemorySearchOptions::default()
            },
            MemoryQueryLimits::default(),
            &cancel
        )),
        "cancelled"
    );
    assert_eq!(
        code(store.project_cancellable(
            MemoryProjectionOptions::default(),
            MemoryQueryLimits::default(),
            &cancel
        )),
        "cancelled"
    );
    assert!(!store.root().exists());
    store
        .secrets
        .register(&["credential-query-fixture".into()])
        .unwrap();
    assert_eq!(
        code(store.search(
            MemorySearchOptions {
                query: "credential-query-fixture".into(),
                ..MemorySearchOptions::default()
            },
            MemoryQueryLimits::default()
        )),
        "sensitive_data"
    );
    let mut original = document();
    original.entries[0].facts[0].text = "A rotated credential value fixture.".into();
    save(&store, &original);
    assert!(
        store
            .project(
                MemoryProjectionOptions::default(),
                MemoryQueryLimits::default()
            )
            .is_ok()
    );
    store
        .secrets
        .register(&["rotated credential value".into()])
        .unwrap();
    assert_eq!(
        code(store.project(
            MemoryProjectionOptions::default(),
            MemoryQueryLimits::default()
        )),
        "sensitive_data"
    );
}

#[test]
fn memory_context_prompt_is_ascii_and_matches_the_documented_snapshot() {
    assert!(MEMORY_CONTEXT_INSTRUCTIONS.is_ascii());
    assert_eq!(
        MEMORY_CONTEXT_INSTRUCTIONS,
        include_str!("../../../../docs/prompt/v1/memory-context.md")
    );
    for marker in [
        "memory_cursor_stale",
        "metadata_withheld",
        "PLAN",
        "Manual Approval",
        "expected revision",
    ] {
        assert!(MEMORY_CONTEXT_INSTRUCTIONS.contains(marker));
    }
}
