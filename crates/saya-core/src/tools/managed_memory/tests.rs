use super::*;
use crate::{
    contract::{HistoryItem, ModelProtocol, Run},
    credentials::SecretRegistry,
    global_memory::{MemoryLimits, WorkspaceIdentity},
};
use serde_json::json;
use std::sync::atomic::AtomicUsize;
use tempfile::TempDir;

struct Gate {
    source: MemoryAuthorizationSource,
    checks: AtomicUsize,
    finishes: AtomicUsize,
    reject_second: bool,
    wrong_binding: bool,
}
impl MemoryGate for Gate {
    fn authorize(
        &self,
        binding: &MemoryAuthorizationBinding,
        _: &ToolContext,
    ) -> Result<MemoryAuthorization, BoundaryError> {
        let previous = self.checks.fetch_add(1, Ordering::AcqRel);
        if self.reject_second && previous == 1 {
            return Err(failure(
                "stale_approval",
                "Memory grant was invalidated before execution",
            ));
        }
        Ok(MemoryAuthorization {
            binding_fingerprint: if self.wrong_binding {
                "wrong_binding".into()
            } else {
                binding.fingerprint.clone()
            },
            source: self.source,
        })
    }
    fn finish(&self) -> Result<(), BoundaryError> {
        self.finishes.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}
struct Fixture {
    _temp: TempDir,
    store: Arc<GlobalMemoryStore>,
    secrets: Arc<SecretRegistry>,
    tools: ManagedMemoryTools,
    session: Session,
    context: ToolContext,
    gate: Arc<Gate>,
}
impl Fixture {
    fn new(source: MemoryAuthorizationSource) -> Self {
        Self::with_gate(Gate {
            source,
            checks: AtomicUsize::new(0),
            finishes: AtomicUsize::new(0),
            reject_second: false,
            wrong_binding: false,
        })
    }
    fn with_gate(gate: Gate) -> Self {
        let temp = TempDir::new().unwrap();
        let secrets = Arc::new(SecretRegistry::default());
        let store = Arc::new(
            GlobalMemoryStore::open(
                &temp.path().join(".saya"),
                WorkspaceIdentity::from_path(temp.path()).unwrap(),
                MemoryLimits::default(),
                secrets.clone(),
            )
            .unwrap(),
        );
        let session = Session {
            id: SessionId::new(),
            workspace: temp.path().into(),
            work_mode: WorkMode::Work,
            profile: "fixture".into(),
            protocol: ModelProtocol::ChatCompletions,
            model: "fixture-model".into(),
            history: vec![HistoryItem::UserText(
                "Prefer concise and accurate explanations.".into(),
            )],
        };
        let context = ToolContext {
            workspace: temp.path().into(),
            run: Run {
                id: RunId::new(),
                session_id: session.id,
                work_mode: WorkMode::Work,
                permissions: PermissionMode::Auto,
            },
        };
        let gate = Arc::new(gate);
        let tools = ManagedMemoryTools::new(
            store.clone(),
            MemoryQueryLimits::default(),
            ManagedMemoryLimits::default(),
            gate.clone(),
        )
        .unwrap();
        Self {
            _temp: temp,
            store,
            secrets,
            tools,
            session,
            context,
            gate,
        }
    }
    fn call(&self, name: &str, arguments: Value) -> ToolCall {
        ToolCall {
            call_id: format!("call_{}", self.gate.checks.load(Ordering::Acquire)),
            name: name.into(),
            arguments: arguments.to_string(),
        }
    }
    fn update(&self, operation_id: &str, scope: &str) -> ToolCall {
        self.call("memory_update", json!({"operation_id":operation_id,"expected_revision":self.store.snapshot().unwrap().revision,"scope":scope,"title":"Communication preferences",
            "facts":[{"id":"fact_user","kind":"explicit_preference","text":"Prefer concise and accurate explanations.","evidence":[{"kind":"user_message","ordinal":1}]}]}))
    }
    fn execute(&self, call: &ToolCall) -> Result<ToolResult, BoundaryError> {
        let cancel = AtomicBool::new(false);
        let prepared = self
            .tools
            .prepare(call, &self.session, &self.context, &cancel)?;
        self.tools
            .execute_prepared(prepared, &self.session, &self.context, &cancel)
    }
    fn tool_evidence(&mut self) {
        self.session.history.push(HistoryItem::ToolCall(ToolCall {
            call_id: "evidence_call".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"guide.md"}"#.into(),
        }));
        self.session.history.push(HistoryItem::ToolResult {
            call_id: "evidence_call".into(),
            result: ToolResult {
                ok: true,
                data: Some(json!({"text":"A verified workspace convention."})),
                error: None,
                truncated: false,
            },
        });
    }
}
fn code<T>(result: Result<T, BoundaryError>) -> String {
    match result {
        Ok(_) => panic!("expected memory boundary error"),
        Err(error) => error.0.code,
    }
}

#[test]
fn user_evidence_produces_host_owned_sources_and_safe_result_ids() {
    let fixture = Fixture::new(MemoryAuthorizationSource::IndependentModelReview);
    let result = fixture
        .execute(&fixture.update("op_first", "global"))
        .unwrap();
    assert!(result.ok);
    let data = result.data.unwrap();
    assert_eq!(data["status"], "committed");
    assert_eq!(data["affected_fact_ids"][0], "fact_user");
    assert!(
        data["affected_entry_ids"][0]
            .as_str()
            .unwrap()
            .starts_with("mem_")
    );
    assert!(
        !data
            .to_string()
            .contains("Prefer concise and accurate explanations")
    );
    let snapshot = fixture.store.snapshot().unwrap();
    assert_eq!(snapshot.entries[0].scope, MemoryScope::Global);
    assert_eq!(
        snapshot.sources[0].kind,
        crate::global_memory::MemorySourceKind::User
    );
    assert!(
        snapshot.sources[0]
            .reference
            .starts_with("session-history:")
    );
    assert_eq!(snapshot.sources[0].observed_at, "not_recorded");
    assert!(snapshot.sources[0].id.starts_with("src_"));
    assert_eq!(fixture.gate.checks.load(Ordering::Acquire), 2);
    assert_eq!(fixture.gate.finishes.load(Ordering::Acquire), 1);
}

#[test]
fn plan_cannot_mutate_with_any_permission_mode_or_an_allowing_gate() {
    for permissions in [
        PermissionMode::Auto,
        PermissionMode::Manual,
        PermissionMode::Bypass,
    ] {
        let mut fixture = Fixture::new(MemoryAuthorizationSource::User);
        fixture.context.run.permissions = permissions;
        fixture.context.run.work_mode = WorkMode::Plan;
        assert_eq!(
            code(fixture.execute(&fixture.update("op_plan", "global"))),
            "mode_denied"
        );
        assert!(!fixture.store.root().exists());
        assert_eq!(fixture.gate.checks.load(Ordering::Acquire), 0);
    }
}

#[test]
fn manual_reads_and_auto_mutations_require_their_actual_authorization_sources() {
    let mut manual = Fixture::new(MemoryAuthorizationSource::Rule);
    manual.context.run.permissions = PermissionMode::Manual;
    let call = manual.call("memory_read", json!({}));
    assert_eq!(code(manual.execute(&call)), "needs_confirmation");
    assert!(!manual.store.root().exists());
    let mut allowed = Fixture::new(MemoryAuthorizationSource::User);
    allowed.context.run.permissions = PermissionMode::Manual;
    assert!(
        allowed
            .execute(&allowed.call("memory_read", json!({})))
            .unwrap()
            .ok
    );
    assert_eq!(allowed.gate.checks.load(Ordering::Acquire), 2);
    assert!(!allowed.store.root().exists());
    let auto = Fixture::new(MemoryAuthorizationSource::Rule);
    assert_eq!(
        code(auto.execute(&auto.update("op_auto_rule", "global"))),
        "needs_confirmation"
    );
    assert!(!auto.store.root().exists());
}

#[test]
fn stale_binding_and_second_gate_denial_produce_no_memory_writes() {
    for (wrong_binding, reject_second) in [(true, false), (false, true)] {
        let fixture = Fixture::with_gate(Gate {
            source: MemoryAuthorizationSource::IndependentModelReview,
            checks: AtomicUsize::new(0),
            finishes: AtomicUsize::new(0),
            wrong_binding,
            reject_second,
        });
        assert_eq!(
            code(fixture.execute(&fixture.update("op_stale", "global"))),
            "stale_approval"
        );
        assert!(!fixture.store.root().exists());
        assert_eq!(fixture.gate.finishes.load(Ordering::Acquire), 1);
    }
}

#[test]
fn modes_permissions_and_persisted_evidence_are_rechecked_after_preparation() {
    let fixture = Fixture::new(MemoryAuthorizationSource::IndependentModelReview);
    let call = fixture.update("op_bound", "global");
    let cancel = AtomicBool::new(false);
    let prepared = fixture
        .tools
        .prepare(&call, &fixture.session, &fixture.context, &cancel)
        .unwrap();
    let mut changed = fixture.context.clone();
    changed.run.work_mode = WorkMode::Plan;
    assert_eq!(
        code(
            fixture
                .tools
                .execute_prepared(prepared, &fixture.session, &changed, &cancel)
        ),
        "mode_denied"
    );
    let prepared = fixture
        .tools
        .prepare(&call, &fixture.session, &fixture.context, &cancel)
        .unwrap();
    let mut session = fixture.session.clone();
    session.history[0] = HistoryItem::UserText("A later correction.".into());
    assert_eq!(
        code(
            fixture
                .tools
                .execute_prepared(prepared, &session, &fixture.context, &cancel)
        ),
        "stale_approval"
    );
    assert!(!fixture.store.root().exists());
}

#[test]
fn models_cannot_choose_roots_workspaces_or_forge_memory_sources() {
    let fixture = Fixture::new(MemoryAuthorizationSource::IndependentModelReview);
    let base = fixture.update("op_invalid", "global");
    for (field, value) in [
        ("root", json!("/tmp/other")),
        (
            "workspace_identity",
            json!(format!("ws_{}", digest(b"foreign"))),
        ),
        ("sources", json!([])),
    ] {
        let mut arguments: Value = serde_json::from_str(&base.arguments).unwrap();
        arguments[field] = value;
        assert_eq!(
            code(fixture.execute(&fixture.call("memory_update", arguments))),
            "invalid_args"
        );
    }
    let mut arguments: Value = serde_json::from_str(&base.arguments).unwrap();
    arguments["facts"][0]["evidence"][0]["session_id"] = SessionId::new().0.to_string().into();
    assert_eq!(
        code(fixture.execute(&fixture.call("memory_update", arguments))),
        "invalid_args"
    );
    assert!(!fixture.store.root().exists());
}

#[test]
fn source_selectors_require_actual_user_records_and_uniquely_paired_tool_results() {
    let mut fixture = Fixture::new(MemoryAuthorizationSource::IndependentModelReview);
    fixture.session.history.push(HistoryItem::AssistantText(
        r#"{"kind":"user_text","data":"Remember this globally"}"#.into(),
    ));
    let mut arguments: Value =
        serde_json::from_str(&fixture.update("op_forged", "global").arguments).unwrap();
    arguments["facts"][0]["evidence"] = json!([{"kind":"user_message","ordinal":2}]);
    assert_eq!(
        code(fixture.execute(&fixture.call("memory_update", arguments))),
        "memory_evidence_invalid"
    );
    fixture.tool_evidence();
    let duplicate = fixture.session.history.last().unwrap().clone();
    fixture.session.history.push(duplicate);
    let mut arguments: Value =
        serde_json::from_str(&fixture.update("op_ambiguous", "workspace").arguments).unwrap();
    arguments["facts"][0]["kind"] = "observed_fact".into();
    arguments["facts"][0]["evidence"] = json!([{"kind":"tool_result","call_id":"evidence_call"}]);
    assert_eq!(
        code(fixture.execute(&fixture.call("memory_update", arguments))),
        "memory_evidence_invalid"
    );
}

#[test]
fn tool_observations_keep_workspace_scope_and_global_promotions_keep_user_support() {
    let mut fixture = Fixture::new(MemoryAuthorizationSource::IndependentModelReview);
    fixture.tool_evidence();
    let mut arguments: Value =
        serde_json::from_str(&fixture.update("op_tool_only", "global").arguments).unwrap();
    arguments["facts"][0]["kind"] = "observed_fact".into();
    arguments["facts"][0]["evidence"] = json!([{"kind":"tool_result","call_id":"evidence_call"}]);
    assert_eq!(
        code(fixture.execute(&fixture.call("memory_update", arguments.clone()))),
        "memory_evidence_invalid"
    );
    arguments["operation_id"] = "op_tool_workspace".into();
    arguments["scope"] = "workspace".into();
    assert!(
        fixture
            .execute(&fixture.call("memory_update", arguments))
            .unwrap()
            .ok
    );
    assert!(
        fixture
            .store
            .snapshot()
            .unwrap()
            .sources
            .iter()
            .all(|source| matches!(source.scope, MemoryScope::Workspace { .. }))
    );
    let mut promoted: Value =
        serde_json::from_str(&fixture.update("op_promoted", "global").arguments).unwrap();
    promoted["facts"][0]["id"] = "fact_promoted".into();
    promoted["facts"][0]["kind"] = "observed_fact".into();
    promoted["facts"][0]["evidence"] = json!([{"kind":"user_message","ordinal":1},{"kind":"tool_result","call_id":"evidence_call"}]);
    let call = fixture.call("memory_update", promoted);
    let cancel = AtomicBool::new(false);
    let prepared = fixture
        .tools
        .prepare(&call, &fixture.session, &fixture.context, &cancel)
        .unwrap();
    let review = fixture.tools.review_metadata(&prepared).unwrap();
    assert_eq!(review["tool_evidence_kept_in_workspace"], true);
    assert!(
        fixture
            .tools
            .execute_prepared(prepared, &fixture.session, &fixture.context, &cancel)
            .unwrap()
            .ok
    );
    let snapshot = fixture.store.snapshot().unwrap();
    let promoted = snapshot
        .entries
        .iter()
        .find(|entry| entry.scope == MemoryScope::Global)
        .unwrap();
    let source_ids = promoted.facts[0]
        .sources
        .iter()
        .map(|source| source.source_id.as_str())
        .collect::<Vec<_>>();
    assert!(
        snapshot
            .sources
            .iter()
            .filter(|source| source_ids.contains(&source.id.as_str()))
            .all(|source| source.kind == crate::global_memory::MemorySourceKind::User)
    );
}

#[test]
fn update_merges_facts_and_idempotent_retries_after_forget_never_reapply_old_text() {
    let mut fixture = Fixture::new(MemoryAuthorizationSource::IndependentModelReview);
    let first = fixture.update("op_original", "global");
    assert!(fixture.execute(&first).unwrap().ok);
    let entry_id = fixture.store.snapshot().unwrap().entries[0].id.clone();
    fixture.session.history.push(HistoryItem::UserText(
        "Use a dark theme independently.".into(),
    ));
    let second = fixture.call("memory_update", json!({"operation_id":"op_independent","expected_revision":fixture.store.snapshot().unwrap().revision,"scope":"global","entry_id":entry_id,
        "facts":[{"id":"fact_theme","kind":"explicit_preference","text":"Use a dark theme.","evidence":[{"kind":"user_message","ordinal":2}]}]}));
    assert!(fixture.execute(&second).unwrap().ok);
    assert_eq!(fixture.store.snapshot().unwrap().entries[0].facts.len(), 2);
    let forget = fixture.call("memory_forget", json!({"operation_id":"op_forget","expected_revision":fixture.store.snapshot().unwrap().revision,"scope":"global","fact_ids":["fact_user"]}));
    let result = fixture.execute(&forget).unwrap();
    assert!(
        !result
            .data
            .unwrap()
            .to_string()
            .contains("Prefer concise and accurate explanations")
    );
    assert_eq!(
        fixture.store.snapshot().unwrap().entries[0].facts[0].id,
        "fact_theme"
    );
    let mut retry = first.clone();
    retry.call_id = "a_new_provider_call_id".into();
    let result = fixture.execute(&retry).unwrap();
    assert_eq!(result.data.as_ref().unwrap()["status"], "already_committed");
    assert_eq!(result.data.as_ref().unwrap()["applied_now"], false);
    assert_eq!(
        fixture.store.snapshot().unwrap().entries[0].facts[0].id,
        "fact_theme"
    );
    assert_eq!(fixture.store.snapshot().unwrap().entries[0].facts.len(), 1);
}

#[test]
fn review_redacts_full_evidence_and_detects_late_credentials_before_a_truncated_excerpt_is_reused()
{
    let mut fixture = Fixture::new(MemoryAuthorizationSource::IndependentModelReview);
    let secret = "memory-review-protected-fixture";
    fixture.session.history[0] = HistoryItem::UserText(format!(
        "Credential {secret}. Prefer concise and accurate explanations."
    ));
    fixture.secrets.register(&[secret.into()]).unwrap();
    let cancel = AtomicBool::new(false);
    let prepared = fixture
        .tools
        .prepare(
            &fixture.update("op_redacted", "global"),
            &fixture.session,
            &fixture.context,
            &cancel,
        )
        .unwrap();
    assert!(
        !fixture
            .tools
            .review_metadata(&prepared)
            .unwrap()
            .to_string()
            .contains(secret)
    );
    assert!(
        fixture
            .tools
            .execute_prepared(prepared, &fixture.session, &fixture.context, &cancel)
            .unwrap()
            .ok
    );
    assert!(
        !std::fs::read_to_string(fixture.store.root().join("MEMORY.md"))
            .unwrap()
            .contains(secret)
    );
    let mut late = Fixture::new(MemoryAuthorizationSource::IndependentModelReview);
    late.tools.limits.max_evidence_excerpt_bytes = 8;
    let future = "a-later-protected-value-fixture";
    late.session.history[0] = HistoryItem::UserText(format!(
        "{future}. Prefer concise and accurate explanations."
    ));
    let prepared = late
        .tools
        .prepare(
            &late.update("op_late", "global"),
            &late.session,
            &late.context,
            &cancel,
        )
        .unwrap();
    late.secrets.register(&[future.into()]).unwrap();
    assert_eq!(
        code(late.tools.review_metadata(&prepared)),
        "stale_approval"
    );
    assert_eq!(
        code(
            late.tools
                .execute_prepared(prepared, &late.session, &late.context, &cancel)
        ),
        "stale_approval"
    );
    assert!(!late.store.root().exists());
}

#[test]
fn cancellation_and_cross_session_context_do_not_initialize_memory() {
    let fixture = Fixture::new(MemoryAuthorizationSource::IndependentModelReview);
    let call = fixture.update("op_cancel", "global");
    assert_eq!(
        code(fixture.tools.prepare(
            &call,
            &fixture.session,
            &fixture.context,
            &AtomicBool::new(true)
        )),
        "cancelled"
    );
    let mut context = fixture.context.clone();
    context.run.session_id = SessionId::new();
    assert_eq!(
        code(
            fixture
                .tools
                .prepare(&call, &fixture.session, &context, &AtomicBool::new(false))
        ),
        "memory_scope"
    );
    assert!(!fixture.store.root().exists());
}

#[test]
fn catalog_exposes_actual_source_selectors_and_definitions_exclude_host_configuration() {
    let mut fixture = Fixture::new(MemoryAuthorizationSource::IndependentModelReview);
    fixture.tool_evidence();
    let catalog = fixture
        .tools
        .evidence_catalog(&fixture.session, &fixture.context, &AtomicBool::new(false))
        .unwrap();
    assert!(
        catalog
            .entries
            .iter()
            .any(|item| item.selector == EvidenceSelector::UserMessage { ordinal: 1 })
    );
    assert!(catalog.entries.iter().any(|item| item.selector
        == EvidenceSelector::ToolResult {
            call_id: "evidence_call".into()
        }));
    let definitions = fixture.tools.definitions();
    assert_eq!(definitions.len(), 5);
    let update = definitions
        .iter()
        .find(|definition| definition.name == "memory_update")
        .unwrap();
    assert!(update.input_schema["properties"].get("root").is_none());
    assert!(update.input_schema["properties"].get("sources").is_none());
    assert_eq!(update.input_schema["additionalProperties"], false);
}

#[test]
fn evidence_pages_keep_original_ordinals_after_projection_and_allow_append_only_pagination() {
    let mut fixture = Fixture::new(MemoryAuthorizationSource::Rule);
    fixture.session.history = vec![HistoryItem::UserText(String::new())];
    for index in 1..=90 {
        fixture
            .session
            .history
            .push(HistoryItem::UserText(format!("Original user fact {index}")));
    }
    // Context compression supplies a shorter provider view while the trusted
    // persisted Session remains the source of all native evidence selectors.
    let mut provider_view = vec![HistoryItem::AssistantText(
        "A compacted view of earlier work.".into(),
    )];
    provider_view.extend(fixture.session.history[88..].iter().cloned());
    let original = fixture.session.history.clone();
    let mut arguments = json!({});
    let mut ordinals = Vec::new();
    let mut pages = 0;
    loop {
        let call = fixture.call("memory_evidence", arguments);
        let result = fixture.execute(&call).unwrap();
        assert!(result.ok);
        let page = result.data.unwrap();
        let entries = page["entries"].as_array().unwrap();
        assert!(entries.len() <= 32);
        for item in entries {
            assert_eq!(item["selector"]["kind"], "user_message");
            assert_eq!(item["state"], "user_input");
            assert_eq!(item["revision"].as_str().unwrap().len(), 64);
            ordinals.push(item["selector"]["ordinal"].as_u64().unwrap());
        }
        pages += 1;
        let cursor = page["next_cursor"].clone();
        if cursor.is_null() {
            break;
        }
        // These real history additions would invalidate a whole-current-history
        // cursor. A fixed-prefix cursor keeps earlier pages accessible.
        let call_id = format!("evidence_page_{pages}");
        fixture
            .session
            .history
            .push(HistoryItem::ToolCall(ToolCall {
                call_id: call_id.clone(),
                name: "memory_evidence".into(),
                arguments: call.arguments,
            }));
        fixture.session.history.push(HistoryItem::ToolResult {
            call_id,
            result: ToolResult {
                ok: true,
                data: Some(page),
                error: None,
                truncated: true,
            },
        });
        arguments = json!({"cursor":cursor});
    }
    assert_eq!(pages, 3);
    assert_eq!(ordinals, (2..=91).rev().collect::<Vec<_>>());
    assert!(ordinals[0] as usize > provider_view.len());
    assert_eq!(
        &fixture.session.history[..original.len()],
        original.as_slice()
    );
    assert!(!fixture.store.root().exists());
}

#[test]
fn evidence_cursor_rejects_modified_prefix_and_other_session_and_prepared_history_changes() {
    let mut fixture = Fixture::new(MemoryAuthorizationSource::Rule);
    fixture.session.history = (0..40)
        .map(|index| HistoryItem::UserText(format!("User evidence {index}")))
        .collect();
    let first = fixture
        .execute(&fixture.call("memory_evidence", json!({})))
        .unwrap()
        .data
        .unwrap();
    let cursor = first["next_cursor"].clone();
    assert!(!cursor.is_null());
    fixture.session.history[0] = HistoryItem::UserText("A changed original source.".into());
    assert_eq!(
        code(fixture.execute(&fixture.call("memory_evidence", json!({"cursor":cursor})))),
        "cursor_invalid"
    );
    let mut other = fixture.session.clone();
    other.id = SessionId::new();
    let mut context = fixture.context.clone();
    context.run.session_id = other.id;
    let call = fixture.call("memory_evidence", json!({"cursor":first["next_cursor"]}));
    let cancel = AtomicBool::new(false);
    let prepared = fixture
        .tools
        .prepare(&call, &other, &context, &cancel)
        .unwrap();
    assert_eq!(
        code(
            fixture
                .tools
                .execute_prepared(prepared, &other, &context, &cancel)
        ),
        "cursor_invalid"
    );
    let call = fixture.call("memory_evidence", json!({}));
    let prepared = fixture
        .tools
        .prepare(&call, &fixture.session, &fixture.context, &cancel)
        .unwrap();
    fixture
        .session
        .history
        .push(HistoryItem::UserText("Appended during approval.".into()));
    assert_eq!(
        code(
            fixture
                .tools
                .execute_prepared(prepared, &fixture.session, &fixture.context, &cancel)
        ),
        "stale_approval"
    );
    assert!(!fixture.store.root().exists());
}

#[test]
fn evidence_is_plan_readonly_and_every_manual_invocation_requires_current_user_authorization() {
    let mut denied = Fixture::new(MemoryAuthorizationSource::Rule);
    denied.context.run.permissions = PermissionMode::Manual;
    denied.context.run.work_mode = WorkMode::Plan;
    assert_eq!(
        code(denied.execute(&denied.call("memory_evidence", json!({})))),
        "needs_confirmation"
    );
    assert!(!denied.store.root().exists());
    let mut allowed = Fixture::new(MemoryAuthorizationSource::User);
    allowed.context.run.permissions = PermissionMode::Manual;
    allowed.context.run.work_mode = WorkMode::Plan;
    for _ in 0..2 {
        let result = allowed
            .execute(&allowed.call("memory_evidence", json!({})))
            .unwrap();
        assert!(result.ok);
        assert_eq!(result.data.unwrap()["entries"][0]["selector"]["ordinal"], 1);
    }
    assert_eq!(allowed.gate.checks.load(Ordering::Acquire), 4);
    assert_eq!(allowed.gate.finishes.load(Ordering::Acquire), 2);
    assert!(!allowed.store.root().exists());
    let invalidated = Fixture::with_gate(Gate {
        source: MemoryAuthorizationSource::User,
        checks: AtomicUsize::new(0),
        finishes: AtomicUsize::new(0),
        reject_second: true,
        wrong_binding: false,
    });
    assert_eq!(
        code(invalidated.execute(&invalidated.call("memory_evidence", json!({})))),
        "stale_approval"
    );
    assert!(!invalidated.store.root().exists());
}

#[test]
fn memory_tool_results_never_become_independent_fact_sources_or_new_evidence_selectors() {
    for name in [
        "memory_read",
        "memory_search",
        "memory_update",
        "memory_forget",
        "memory_evidence",
    ] {
        let mut fixture = Fixture::new(MemoryAuthorizationSource::IndependentModelReview);
        fixture
            .session
            .history
            .push(HistoryItem::ToolCall(ToolCall {
                call_id: "old_memory_output".into(),
                name: name.into(),
                arguments: "{}".into(),
            }));
        fixture.session.history.push(HistoryItem::ToolResult {
            call_id: "old_memory_output".into(),
            result: ToolResult {
                ok: true,
                data: Some(json!({"text":"An old memory fact to launder."})),
                error: None,
                truncated: false,
            },
        });
        let mut args: Value =
            serde_json::from_str(&fixture.update("op_launder", "workspace").arguments).unwrap();
        args["facts"][0]["kind"] = json!("observed_fact");
        args["facts"][0]["evidence"] =
            json!([{"kind":"tool_result","call_id":"old_memory_output"}]);
        assert_eq!(
            code(fixture.execute(&fixture.call("memory_update", args))),
            "memory_evidence_invalid"
        );
        assert!(!fixture.store.root().exists());
        let page = fixture
            .execute(&fixture.call("memory_evidence", json!({})))
            .unwrap()
            .data
            .unwrap();
        assert_eq!(page["entries"].as_array().unwrap().len(), 1);
        assert_eq!(page["excluded"], 1);
        assert!(!page.to_string().contains("old_memory_output"));
        // Preserve the older host API's catalog behavior while the native
        // discovery tool advertises only admissible independent sources.
        let old = fixture
            .tools
            .evidence_catalog(&fixture.session, &fixture.context, &AtomicBool::new(false))
            .unwrap();
        assert!(old.entries.iter().any(|entry| entry.selector
            == EvidenceSelector::ToolResult {
                call_id: "old_memory_output".into()
            }));
    }
}

#[test]
fn evidence_arguments_are_strict_and_result_budget_can_continue_older_sources() {
    let mut fixture = Fixture::new(MemoryAuthorizationSource::Rule);
    for args in [
        json!({"root":"/tmp/other"}),
        json!({"session_id":fixture.session.id}),
        json!({"workspace":"/"}),
        json!({"max_items":0}),
        json!({"max_items":33}),
    ] {
        assert_eq!(
            code(fixture.execute(&fixture.call("memory_evidence", args))),
            "invalid_args"
        );
    }
    fixture.session.history = (0..70)
        .map(|index| HistoryItem::UserText(format!("User evidence {index}")))
        .collect();
    fixture.tools.limits.max_result_bytes = 1024;
    let first = fixture
        .execute(&fixture.call("memory_evidence", json!({})))
        .unwrap();
    assert!(encode(&first, 1024).is_ok());
    let data = first.data.unwrap();
    assert!(!data["entries"].as_array().unwrap().is_empty());
    assert!(!data["next_cursor"].is_null());
    let next = fixture
        .execute(&fixture.call("memory_evidence", json!({"cursor":data["next_cursor"]})))
        .unwrap();
    assert!(encode(&next, 1024).is_ok());
    assert!(
        next.data.unwrap()["entries"][0]["selector"]["ordinal"]
            .as_u64()
            .unwrap()
            < data["entries"][0]["selector"]["ordinal"].as_u64().unwrap()
    );
    assert!(!fixture.store.root().exists());
}

#[test]
fn evidence_reports_actual_success_failure_unknown_and_truncation_without_source_content() {
    let mut fixture = Fixture::new(MemoryAuthorizationSource::Rule);
    for (index, state) in [
        EvidenceState::Succeeded,
        EvidenceState::Failed,
        EvidenceState::Unknown,
    ]
    .iter()
    .enumerate()
    {
        let call_id = format!("observation_{index}");
        fixture
            .session
            .history
            .push(HistoryItem::ToolCall(ToolCall {
                call_id: call_id.clone(),
                name: "exec".into(),
                arguments: "{}".into(),
            }));
        fixture.session.history.push(HistoryItem::ToolResult {
            call_id,
            result: ToolResult {
                ok: *state == EvidenceState::Succeeded,
                data: Some(
                    json!({"body":"Detailed source content stays out of the metadata page."}),
                ),
                error: match state {
                    EvidenceState::Succeeded => None,
                    EvidenceState::Unknown => Some(failure("outcome_unknown", "Unconfirmed").0),
                    _ => Some(failure("command_failed", "Failed").0),
                },
                truncated: index == 2,
            },
        });
    }
    let result = fixture
        .execute(&fixture.call("memory_evidence", json!({})))
        .unwrap();
    let page = result.data.unwrap();
    let entries = page["entries"].as_array().unwrap();
    assert_eq!(entries[0]["state"], "unknown");
    assert_eq!(entries[0]["result_truncated"], true);
    assert_eq!(entries[1]["state"], "failed");
    assert_eq!(entries[2]["state"], "succeeded");
    assert!(!page.to_string().contains("Detailed source content"));
    assert!(!fixture.store.root().exists());
}

#[test]
fn indexed_topic_pages_are_bounded_and_paths_and_versions_remain_host_checked() {
    let fixture = Fixture::new(MemoryAuthorizationSource::IndependentModelReview);
    assert!(
        fixture
            .execute(&fixture.update("op_topic", "global"))
            .unwrap()
            .ok
    );
    let path = fixture.store.root().join("MEMORY.md");
    let mut document = fixture
        .store
        .parse_document(&std::fs::read_to_string(&path).unwrap())
        .unwrap();
    document.entries[0].resources.push("memory/topic.md".into());
    std::fs::write(&path, fixture.store.render_document(&document).unwrap()).unwrap();
    let text = "中文🙂 longer topic detail.\n".repeat(200);
    std::fs::write(fixture.store.root().join("memory/topic.md"), &text).unwrap();
    let revision = fixture.store.snapshot().unwrap().revision;
    let mut cursor = Value::Null;
    let mut collected = String::new();
    for _ in 0..100 {
        let mut arguments =
            json!({"topic_path":"memory/topic.md","expected_revision":revision,"max_bytes":1600});
        if !cursor.is_null() {
            arguments["cursor"] = cursor;
        }
        let result = fixture
            .execute(&fixture.call("memory_read", arguments))
            .unwrap();
        let data = result.data.unwrap();
        assert!(serde_json::to_vec(&data).unwrap().len() <= 1600);
        collected.push_str(data["text"].as_str().unwrap());
        cursor = data["next_cursor"].clone();
        if cursor.is_null() {
            break;
        }
    }
    assert_eq!(collected, text);
    assert_eq!(
        code(fixture.execute(&fixture.call(
            "memory_read",
            json!({"topic_path":"memory/../outside.md","expected_revision":revision})
        ))),
        "invalid_args"
    );
    assert_eq!(
        code(fixture.execute(&fixture.call(
            "memory_read",
            json!({"topic_path":"memory/unknown.md","expected_revision":revision})
        ))),
        "memory_not_found"
    );
}

#[test]
fn unknown_publication_result_preserves_safe_receipt_and_signals_stop_without_replay() {
    let fixture = Fixture::new(MemoryAuthorizationSource::IndependentModelReview);
    let outcome = crate::global_memory::MemoryCommitOutcome {
        status: MemoryCommitStatus::Unknown,
        receipt: MemoryOperationReceipt {
            operation_id: "op_pending".into(),
            change_digest: digest(b"change"),
            generation: 1,
            before_revision: digest(b"before"),
            document_digest: digest(b"after"),
            host_request_digest: Some(digest(b"request")),
        },
        revision: None,
        error: Some(failure("memory_io", "Memory durability requires recovery").0),
        historical_copies_retained: false,
        projection_refresh_required: true,
    };
    let error = fixture
        .tools
        .mutation_result(outcome, MutationSummary::default())
        .unwrap_err();
    assert_eq!(error.0.code, "outcome_unknown");
    let observed = error.2.unwrap();
    assert!(!observed.ok);
    assert_eq!(observed.data.as_ref().unwrap()["status"], "unknown");
    assert_eq!(
        observed.data.as_ref().unwrap()["operation_id"],
        "op_pending"
    );
    assert!(!fixture.store.root().exists());
}
