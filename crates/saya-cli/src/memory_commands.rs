//! Explicit offline memory administration. The caller owns scheduling, printing
//! and exit-code mapping. Every storage operation uses GlobalMemoryStore.
use clap::{Subcommand, ValueEnum};
use saya_core::{
    config::Config,
    contract::{BoundaryError, ErrorInfo, WorkMode},
    credentials::SecretRegistry,
    global_memory::{
        GlobalMemoryStore, MemoryBackupV1, MemoryChange, MemoryCommitOutcome, MemoryCommitStatus,
        MemoryCursor, MemoryFact, MemoryLimits, MemoryProposal, MemoryQueryLimits,
        MemoryReadOptions, MemoryScope, MemorySearchOptions, WorkspaceIdentity,
    },
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

mod file;
#[cfg(test)]
mod tests;

const PAGE_SIZE: usize = 20;
const MAX_PAGE_SIZE: usize = 100;
const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, ValueEnum)]
pub enum MemoryScopeOption {
    Global,
    Workspace,
}
impl MemoryScopeOption {
    fn resolve(self, store: &GlobalMemoryStore) -> MemoryScope {
        match self {
            Self::Global => MemoryScope::Global,
            Self::Workspace => MemoryScope::Workspace {
                identity: store.workspace().id.clone(),
            },
        }
    }
}
#[derive(Clone, Subcommand)]
pub enum MemoryAction {
    /// Show effective memory feature settings and the explicitly resolved root.
    Settings,
    /// List the current visible global and workspace entry metadata.
    List {
        #[arg(long, default_value_t = 1)]
        page: u32,
        #[arg(long, default_value_t = PAGE_SIZE)]
        page_size: usize,
    },
    /// Show current memory and forgetting revisions without changing the store.
    Version,
    /// Read bounded current fact pages using their stable IDs and cursor.
    Show {
        #[arg(long)]
        entry_id: Option<String>,
        #[arg(long)]
        fact_id: Option<String>,
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long, default_value_t = 16384)]
        max_bytes: usize,
        #[arg(long, default_value_t = 16)]
        max_items: usize,
    },
    /// Search current memory text and aliases within the configured workspace.
    Search {
        query: String,
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long, default_value_t = 16384)]
        max_bytes: usize,
        #[arg(long, default_value_t = 16)]
        max_items: usize,
    },
    /// Inspect a bounded diff for one explicit JSON MemoryChange.
    Diff {
        #[arg(long)]
        change: PathBuf,
        #[arg(long)]
        expected_revision: String,
        #[arg(long)]
        operation_id: String,
        #[arg(long, default_value_t = 1)]
        page: u32,
        #[arg(long, default_value_t = 10)]
        page_size: usize,
    },
    /// Review a version-bound explicit JSON MemoryChange before applying it.
    Review {
        #[arg(long)]
        change: PathBuf,
        #[arg(long)]
        expected_revision: String,
        #[arg(long)]
        operation_id: String,
        #[arg(long, default_value_t = 1)]
        page: u32,
        #[arg(long, default_value_t = 10)]
        page_size: usize,
    },
    /// Apply an explicit human-selected JSON change to its exact current version.
    Apply {
        #[arg(long)]
        change: PathBuf,
        #[arg(long)]
        expected_revision: String,
        #[arg(long)]
        operation_id: String,
    },
    /// Forget stable fact IDs in the declared visible scope.
    Forget {
        #[arg(long, value_enum)]
        scope: MemoryScopeOption,
        #[arg(long = "fact-id", required = true)]
        fact_ids: Vec<String>,
        #[arg(long)]
        expected_revision: String,
        #[arg(long)]
        operation_id: String,
    },
    /// List retained index versions and their recorded generations.
    History {
        #[arg(long, default_value_t = 1)]
        page: u32,
        #[arg(long, default_value_t = PAGE_SIZE)]
        page_size: usize,
    },
    /// Restore a retained index version while preserving current forget state.
    Rollback {
        version: String,
        #[arg(long)]
        expected_revision: String,
        #[arg(long)]
        operation_id: String,
    },
    /// Export the current active view, source metadata and forgetting constraints.
    Export {
        #[arg(long)]
        file: PathBuf,
    },
    /// Atomically restore an active-view JSON backup with matching topic references.
    Restore {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        expected_revision: String,
        #[arg(long)]
        operation_id: String,
    },
    /// Inspect a durable operation receipt and the current view revision.
    Operation { operation_id: String },
    /// Finish an existing durable memory publication without replaying its input.
    Recover,
}
impl MemoryAction {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::List { .. } => "list",
            Self::Version => "version",
            Self::Show { .. } => "show",
            Self::Search { .. } => "search",
            Self::Diff { .. } => "diff",
            Self::Review { .. } => "review",
            Self::Apply { .. } => "apply",
            Self::Forget { .. } => "forget",
            Self::History { .. } => "history",
            Self::Rollback { .. } => "rollback",
            Self::Export { .. } => "export",
            Self::Restore { .. } => "restore",
            Self::Operation { .. } => "operation",
            Self::Recover => "recover",
        }
    }
    fn memory_write(&self) -> bool {
        matches!(
            self,
            Self::Apply { .. }
                | Self::Forget { .. }
                | Self::Rollback { .. }
                | Self::Restore { .. }
                | Self::Recover
        )
    }
    fn file_write(&self) -> bool {
        self.memory_write() || matches!(self, Self::Export { .. })
    }
    fn content_read(&self) -> bool {
        matches!(
            self,
            Self::List { .. }
                | Self::Show { .. }
                | Self::Search { .. }
                | Self::Diff { .. }
                | Self::Review { .. }
                | Self::Export { .. }
        )
    }
}
fn failure(code: &str, message: &str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}
fn check_cancel(cancel: &AtomicBool) -> Result<(), BoundaryError> {
    if cancel.load(Ordering::Acquire) {
        return Err(failure("cancelled", "Memory administration cancelled"));
    }
    Ok(())
}
fn check_digest(value: &str) -> Result<(), BoundaryError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(failure(
            "invalid_args",
            "Memory revision must be a complete lowercase SHA-256 digest",
        ));
    }
    Ok(())
}
fn known_registry(config: &Config) -> Result<Arc<SecretRegistry>, BoundaryError> {
    let values = config
        .secret_env_names()
        .iter()
        .map(|name| config.secret(name).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    let registry = Arc::new(SecretRegistry::default());
    registry.register(&values)?;
    Ok(registry)
}
fn safe_output(mut value: Value, secrets: &SecretRegistry) -> Result<Value, BoundaryError> {
    let known = secrets.snapshot()?;
    let encoded = serde_json::to_vec(&value).map_err(|_| {
        failure(
            "output_error",
            "Memory administration output cannot be encoded",
        )
    })?;
    if encoded.len() > MAX_OUTPUT_BYTES {
        return Err(failure(
            "memory_output_limit",
            "Memory administration output exceeds its limit; use a smaller page",
        ));
    }
    saya_core::redaction::redact(&mut value, &known)?;
    let encoded = serde_json::to_string(&value).map_err(|_| {
        failure(
            "output_error",
            "Memory administration output cannot be encoded",
        )
    })?;
    if encoded.len() > MAX_OUTPUT_BYTES || saya_core::redaction::contains_secret(&encoded, &known) {
        return Err(failure(
            "sensitive_data",
            "Memory administration output was withheld",
        ));
    }
    Ok(value)
}
fn page_bounds(
    total: usize,
    page: u32,
    page_size: usize,
    maximum: usize,
) -> Result<(usize, usize), BoundaryError> {
    if page == 0 || page_size == 0 || page_size > maximum {
        return Err(failure(
            "invalid_args",
            "Memory page and page size exceed their bounds",
        ));
    }
    let start = (page as usize - 1)
        .checked_mul(page_size)
        .ok_or_else(|| failure("invalid_args", "Memory page offset overflows"))?;
    if (total == 0 && page > 1) || (total > 0 && start >= total) {
        return Err(failure(
            "invalid_args",
            "Memory page is beyond the current result set",
        ));
    }
    Ok((start, start.saturating_add(page_size).min(total)))
}
fn cursor(value: &Option<String>) -> Result<Option<MemoryCursor>, BoundaryError> {
    value
        .as_ref()
        .map(|value| {
            if value.len() > 1024 {
                return Err(failure(
                    "invalid_args",
                    "Memory cursor exceeds its byte limit",
                ));
            }
            serde_json::from_str(value)
                .map_err(|_| failure("invalid_args", "Memory cursor JSON is invalid"))
        })
        .transpose()
}
fn change_file(
    path: &Path,
    config: &Config,
    secrets: &SecretRegistry,
    cancel: &AtomicBool,
) -> Result<MemoryChange, BoundaryError> {
    let text = file::read(path, config, MAX_INPUT_BYTES, secrets, cancel)?;
    serde_json::from_str(&text)
        .map_err(|_| failure("memory_format", "Memory change JSON is invalid"))
}
fn request_digest(
    store: &GlobalMemoryStore,
    expected_revision: &str,
    operation_id: &str,
    change: &MemoryChange,
) -> Result<String, BoundaryError> {
    check_digest(expected_revision)?;
    store.administrative_request_digest(
        &json!({"expected_revision":expected_revision,"operation_id":operation_id,"change":change}),
    )
}
fn existing(
    store: &GlobalMemoryStore,
    operation_id: &str,
    request_digest: &str,
) -> Result<Option<MemoryCommitOutcome>, BoundaryError> {
    let Some(receipt) = store.operation_receipt(operation_id)? else {
        return Ok(None);
    };
    if receipt.host_request_digest.as_deref() != Some(request_digest) {
        return Err(failure(
            "memory_idempotency_conflict",
            "Memory operation identity belongs to another administrative request",
        ));
    }
    Ok(Some(MemoryCommitOutcome {
        status: MemoryCommitStatus::AlreadyCommitted,
        receipt,
        revision: Some(store.snapshot()?.revision),
        error: None,
        historical_copies_retained: !store.history()?.is_empty(),
        projection_refresh_required: true,
    }))
}
fn apply(
    store: &GlobalMemoryStore,
    expected_revision: &str,
    operation_id: &str,
    change: MemoryChange,
    cancel: &AtomicBool,
) -> Result<Value, BoundaryError> {
    check_cancel(cancel)?;
    let digest = request_digest(store, expected_revision, operation_id, &change)?;
    let outcome = match existing(store, operation_id, &digest)? {
        Some(outcome) => outcome,
        None => {
            let proposal =
                store.propose_host_request(expected_revision, operation_id, &digest, change)?;
            check_cancel(cancel)?;
            store.commit_cancellable(&proposal, cancel)?
        }
    };
    commit_output(outcome)
}
fn commit_output(outcome: MemoryCommitOutcome) -> Result<Value, BoundaryError> {
    let value = json!({"status":outcome.status,"applied_now":outcome.status==MemoryCommitStatus::Committed,"receipt":outcome.receipt,
        "current_revision":outcome.revision,"projection_refresh_required":outcome.projection_refresh_required,
        "historical_copies_retained":outcome.historical_copies_retained,"retention_policy":{"topic_bodies":"retained_when_links_are_removed","original_evidence":"source_owner","existing_exports":"export_owner"},
        "detail":outcome.error,"content_state":"Read current memory for current contents"});
    if outcome.status == MemoryCommitStatus::Unknown {
        return Err(BoundaryError(
            failure(
                "outcome_unknown",
                "Memory publication requires recovery and was not replayed",
            )
            .0,
            None,
            Some(Box::new(saya_core::contract::ToolResult {
                ok: false,
                data: Some(value),
                error: None,
                truncated: false,
            })),
        ));
    }
    Ok(value)
}
fn fact_preview(fact: &MemoryFact) -> Value {
    let mut end = 2048.min(fact.text.len());
    while !fact.text.is_char_boundary(end) {
        end -= 1;
    }
    json!({"id":fact.id,"kind":fact.kind,"text":&fact.text[..end],"text_complete":end==fact.text.len(),"sources":fact.sources,
        "aliases":fact.aliases.iter().take(8).collect::<Vec<_>>(),"aliases_omitted":fact.aliases.len().saturating_sub(8),"derived_from":fact.derived_from})
}
fn review_output(
    store: &GlobalMemoryStore,
    proposal: &MemoryProposal,
    digest: &str,
    page: u32,
    page_size: usize,
) -> Result<Value, BoundaryError> {
    let before = proposal
        .before_entries
        .iter()
        .flat_map(|entry| {
            entry
                .facts
                .iter()
                .map(move |fact| (fact.id.as_str(), (entry, fact)))
        })
        .collect::<BTreeMap<_, _>>();
    let after = proposal
        .after_entries
        .iter()
        .flat_map(|entry| {
            entry
                .facts
                .iter()
                .map(move |fact| (fact.id.as_str(), (entry, fact)))
        })
        .collect::<BTreeMap<_, _>>();
    let ids = before
        .keys()
        .chain(after.keys())
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|id| before.get(id).map(|(_, fact)| fact) != after.get(id).map(|(_, fact)| fact))
        .collect::<Vec<_>>();
    let (start, end) = page_bounds(ids.len(), page, page_size, 10)?;
    let changes = ids[start..end].iter().map(|id| {
        let old = before.get(id).copied();
        let new = after.get(id).copied();
        json!({"fact_id":id,"scope":new.or(old).map(|(entry,_)| &entry.scope),"before":old.map(|(_,fact)|fact_preview(fact)),"after":new.map(|(_,fact)|fact_preview(fact))})
    }).collect::<Vec<_>>();
    let mut entries = Vec::new();
    for entry in proposal
        .after_entries
        .iter()
        .chain(&proposal.before_entries)
    {
        if entries
            .iter()
            .any(|item: &Value| item["entry_id"] == entry.id)
        {
            continue;
        }
        let old = proposal
            .before_entries
            .iter()
            .find(|old| old.id == entry.id);
        let new = proposal.after_entries.iter().find(|new| new.id == entry.id);
        if old.map(|entry| (&entry.title, &entry.updated_at))
            != new.map(|entry| (&entry.title, &entry.updated_at))
        {
            entries.push(json!({"entry_id":entry.id,"scope":entry.scope,"before_title":old.map(|entry| &entry.title),"after_title":new.map(|entry| &entry.title),
                "before_updated_at":old.map(|entry| &entry.updated_at),"after_updated_at":new.map(|entry| &entry.updated_at)}));
        }
    }
    let value = json!({"operation_id":proposal.operation_id,"expected_revision":proposal.expected_revision,"request_digest":digest,
        "proposed_document_digest":proposal.proposed_document_digest,"executed":false,"page":page,"page_size":page_size,"total_changes":ids.len(),
        "changes":changes,"entry_metadata_changes":entries,"next_page":(end<ids.len()).then_some(page+1)});
    store.redact_review_value(value)
}

/// Perform one explicit offline command. The caller should run this blocking
/// filesystem work on its bounded administration worker.
#[cfg(test)]
pub fn run(action: &MemoryAction, config: &Config) -> Result<Value, BoundaryError> {
    run_cancellable(action, config, &AtomicBool::new(false))
}
/// Complete one explicit administration operation using the caller's stop flag.
/// After publication, return its observed receipt even if cancellation arrives.
/// The caller waits for this worker and owns signal exit-code reporting.
pub fn run_cancellable(
    action: &MemoryAction,
    config: &Config,
    cancel: &AtomicBool,
) -> Result<Value, BoundaryError> {
    check_cancel(cancel)?;
    if action.content_read() && !config.memory.read {
        return Err(failure(
            "memory_read_disabled",
            "Memory reading is disabled in the startup configuration",
        ));
    }
    if action.memory_write() && !config.memory.write {
        return Err(failure(
            "memory_write_disabled",
            "Memory writing is disabled in the startup configuration",
        ));
    }
    if action.file_write() && config.mode == WorkMode::Plan {
        return Err(failure(
            "mode_denied",
            "PLAN permits read-only memory administration",
        ));
    }
    let secrets = known_registry(config)?;
    let root = config.memory_root()?;
    let store = GlobalMemoryStore::open(
        &root,
        WorkspaceIdentity::from_path(&config.cwd)?,
        MemoryLimits::default(),
        secrets.clone(),
    )?;
    let data = match action {
        MemoryAction::Settings => {
            json!({"root":root,"read":config.memory.read,"inject":config.memory.inject,"write":config.memory.write,"work_mode":config.mode})
        }
        MemoryAction::Version => {
            let snapshot = store.snapshot_cancellable(cancel)?;
            json!({"exists":snapshot.exists,"revision":snapshot.revision,"forget_revision":snapshot.forget_revision,"workspace":snapshot.workspace})
        }
        MemoryAction::List { page, page_size } => {
            let snapshot = store.snapshot_cancellable(cancel)?;
            let (start, end) =
                page_bounds(snapshot.entries.len(), *page, *page_size, MAX_PAGE_SIZE)?;
            let entries = snapshot.entries[start..end].iter().map(|entry| json!({"entry_id":entry.id,"title":entry.title,"scope":entry.scope,"updated_at":entry.updated_at,
                "facts":entry.facts.len(),"resources":entry.resources.len()})).collect::<Vec<_>>();
            json!({"revision":snapshot.revision,"forget_revision":snapshot.forget_revision,"page":page,"page_size":page_size,"total":snapshot.entries.len(),"entries":entries,"next_page":(end<snapshot.entries.len()).then_some(page+1)})
        }
        MemoryAction::Show {
            entry_id,
            fact_id,
            cursor: position,
            max_bytes,
            max_items,
        } => serde_json::to_value(store.read_page_cancellable(
            MemoryReadOptions {
                entry_id: entry_id.clone(),
                fact_id: fact_id.clone(),
                cursor: cursor(position)?,
                max_bytes: *max_bytes,
                max_items: *max_items,
            },
            MemoryQueryLimits::default(),
            cancel,
        )?)
        .map_err(|_| failure("output_error", "Memory read result cannot be encoded"))?,
        MemoryAction::Search {
            query,
            cursor: position,
            max_bytes,
            max_items,
        } => serde_json::to_value(store.search_cancellable(
            MemorySearchOptions {
                query: query.clone(),
                cursor: cursor(position)?,
                max_bytes: *max_bytes,
                max_items: *max_items,
            },
            MemoryQueryLimits::default(),
            cancel,
        )?)
        .map_err(|_| failure("output_error", "Memory search result cannot be encoded"))?,
        MemoryAction::Diff {
            change,
            expected_revision,
            operation_id,
            page,
            page_size,
        }
        | MemoryAction::Review {
            change,
            expected_revision,
            operation_id,
            page,
            page_size,
        } => {
            let change = change_file(change, config, &secrets, cancel)?;
            let digest = request_digest(&store, expected_revision, operation_id, &change)?;
            if let Some(outcome) = existing(&store, operation_id, &digest)? {
                commit_output(outcome)?
            } else {
                let proposal =
                    store.propose_host_request(expected_revision, operation_id, &digest, change)?;
                {
                    check_cancel(cancel)?;
                    review_output(&store, &proposal, &digest, *page, *page_size)?
                }
            }
        }
        MemoryAction::Apply {
            change,
            expected_revision,
            operation_id,
        } => apply(
            &store,
            expected_revision,
            operation_id,
            change_file(change, config, &secrets, cancel)?,
            cancel,
        )?,
        MemoryAction::Forget {
            scope,
            fact_ids,
            expected_revision,
            operation_id,
        } => apply(
            &store,
            expected_revision,
            operation_id,
            MemoryChange::Forget {
                scope: scope.resolve(&store),
                fact_ids: fact_ids.clone(),
            },
            cancel,
        )?,
        MemoryAction::History { page, page_size } => {
            let versions = store.history()?;
            let (start, end) = page_bounds(versions.len(), *page, *page_size, MAX_PAGE_SIZE)?;
            json!({"page":page,"page_size":page_size,"total":versions.len(),"versions":&versions[start..end],"next_page":(end<versions.len()).then_some(page+1),"scope":"retained_index_versions"})
        }
        MemoryAction::Rollback {
            version,
            expected_revision,
            operation_id,
        } => {
            check_digest(version)?;
            apply(
                &store,
                expected_revision,
                operation_id,
                MemoryChange::RestoreVersion {
                    version: version.clone(),
                },
                cancel,
            )?
        }
        MemoryAction::Export { file: target } => {
            let backup = store.export_backup()?;
            let bytes = serde_json::to_vec(&backup)
                .map_err(|_| failure("output_error", "Memory backup cannot be encoded"))?;
            let receipt = file::publish(target, &bytes, config, &secrets, cancel)?;
            json!({"publication":receipt,"source_revision":backup.source_revision,"forget_revision":backup.forget_revision,"coverage":backup.coverage,"backup_checksum":backup.checksum})
        }
        MemoryAction::Restore {
            file: source,
            expected_revision,
            operation_id,
        } => {
            let text = file::read(source, config, MAX_INPUT_BYTES, &secrets, cancel)?;
            let backup: MemoryBackupV1 = store.parse_backup(&text)?;
            apply(
                &store,
                expected_revision,
                operation_id,
                MemoryChange::RestoreDocument { backup },
                cancel,
            )?
        }
        MemoryAction::Operation { operation_id } => {
            json!({"receipt":store.operation_receipt(operation_id)?,"current_revision":store.snapshot()?.revision,"content_state":"Read current memory for current contents"})
        }
        MemoryAction::Recover => {
            let receipt = store.recover_cancellable(cancel)?;
            json!({"recovered_operation":receipt,"current_revision":store.snapshot()?.revision,"replayed_input":false})
        }
    };
    if !action.file_write() {
        check_cancel(cancel)?;
    }
    safe_output(
        json!({"schema_version":1,"kind":"memory","action":action.name(),"data":data}),
        &secrets,
    )
}
