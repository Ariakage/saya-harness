//! Explicit user-side administration. Queries and proposals use the same store
//! as runtime memory. Prepared actions are single-use and retain approved bytes.
use saya_core::{
    boundary::Cancellation,
    config::Config,
    contract::{BoundaryError, ErrorInfo, RunId, WorkMode},
    credentials::SecretRegistry,
    global_memory::{
        GlobalMemoryStore, MemoryChange, MemoryCommitStatus, MemoryKind, MemoryLimits,
        MemoryProposal, MemoryQueryLimits, MemoryReadOptions, MemorySearchOptions, MemorySource,
        MemorySourceKind, MemorySourceRef, WorkspaceIdentity, new_source_id,
    },
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

mod file;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_export;

pub const MAX_VIEW_BYTES: usize = 64 * 1024;
const PAGE_ITEMS: usize = 12;
const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;

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
    if cancel.load(Ordering::SeqCst) {
        return Err(failure("cancelled", "Memory administration cancelled"));
    }
    Ok(())
}

/// Shared, dynamically updated credentials must be supplied by the caller.
pub struct MemoryAdmin {
    config: Config,
    store: GlobalMemoryStore,
    secrets: Arc<SecretRegistry>,
}

/// The constructor is private. A UI can display the review but cannot replace
/// the proposal, source version, export path or captured bytes after approval.
pub struct PreparedMemoryAction {
    root: PathBuf,
    workspace: WorkspaceIdentity,
    expected_revision: String,
    operation_id: String,
    review: Value,
    action: PreparedAction,
}
enum PreparedAction {
    Change(Box<MemoryProposal>),
    Export {
        target: Box<crate::export::PreparedTarget>,
        bytes: Vec<u8>,
    },
}
impl PreparedMemoryAction {
    pub fn review(&self) -> &Value {
        &self.review
    }
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }
    pub fn expected_revision(&self) -> &str {
        &self.expected_revision
    }
}

impl MemoryAdmin {
    pub fn open(config: &Config, secrets: Arc<SecretRegistry>) -> Result<Self, BoundaryError> {
        secrets.register(
            &config
                .secret_env_names()
                .iter()
                .map(|name| config.secret(name).map(str::to_owned))
                .collect::<Result<Vec<_>, _>>()?,
        )?;
        let store = GlobalMemoryStore::open(
            &config.memory_root()?,
            WorkspaceIdentity::from_path(&config.cwd)?,
            MemoryLimits::default(),
            secrets.clone(),
        )?;
        Ok(Self {
            config: config.clone(),
            store,
            secrets,
        })
    }
    pub fn settings(&self) -> Result<Value, BoundaryError> {
        self.safe(json!({"root":self.store.root(),"workspace":self.store.workspace(),"read":self.config.memory.read,
            "inject":self.config.memory.inject,"write":self.config.memory.write,"mode":self.config.mode,
            "settings_lifetime":"current process"}))
    }
    fn readable(&self) -> Result<(), BoundaryError> {
        if !self.config.memory.read {
            return Err(failure(
                "memory_read_disabled",
                "Memory reading is disabled",
            ));
        }
        Ok(())
    }
    fn writable(&self, export: bool) -> Result<(), BoundaryError> {
        if !self.config.memory.read {
            return Err(failure(
                "memory_read_disabled",
                "Memory review requires reading; enable read before preparing or confirming a change",
            ));
        }
        if self.config.mode == WorkMode::Plan {
            return Err(failure(
                "mode_denied",
                "PLAN permits read-only memory administration",
            ));
        }
        if !export && !self.config.memory.write {
            return Err(failure(
                "memory_write_disabled",
                "Memory writing is disabled",
            ));
        }
        Ok(())
    }
    fn safe(&self, mut value: Value) -> Result<Value, BoundaryError> {
        let encoded = serde_json::to_vec(&value)
            .map_err(|_| failure("memory_output", "Memory view cannot be encoded"))?;
        if encoded.len() > MAX_VIEW_BYTES {
            return Err(failure(
                "memory_view_limit",
                "Memory view exceeds its bound; choose a smaller selection",
            ));
        }
        saya_core::redaction::redact(&mut value, &self.secrets.snapshot()?)?;
        if serde_json::to_vec(&value).map_or(true, |bytes| bytes.len() > MAX_VIEW_BYTES) {
            return Err(failure(
                "memory_view_limit",
                "Memory view exceeds its byte limit",
            ));
        }
        Ok(value)
    }
    /// Bounded current view. Read/search cursors bind the original query and
    /// revision, while numbered metadata pages always show their new revision.
    pub fn query(&self, command: &str, cancel: &AtomicBool) -> Result<Value, BoundaryError> {
        self.readable()?;
        check_cancel(cancel)?;
        let (verb, argument) = command
            .trim()
            .split_once(' ')
            .unwrap_or((command.trim(), ""));
        let argument = argument.trim();
        let data = match verb {
            "" | "list" => {
                let snapshot = self.store.snapshot_cancellable(cancel)?;
                let (start, end, page) = page(snapshot.entries.len(), argument)?;
                json!({"revision":snapshot.revision,"forget_revision":snapshot.forget_revision,"page":page,
                    "total":snapshot.entries.len(),"next_page":(end<snapshot.entries.len()).then_some(page+1),
                    "entries":snapshot.entries[start..end].iter().map(|entry|json!({"id":entry.id,"title":entry.title,"scope":entry.scope,"updated_at":entry.updated_at,"facts":entry.facts.len(),"resources":entry.resources})).collect::<Vec<_>>()})
            }
            "show" => {
                let options = if argument.starts_with('{') {
                    serde_json::from_str::<MemoryReadOptions>(argument).map_err(|_| {
                        failure("invalid_args", "Use show ID or show with read-options JSON")
                    })?
                } else if argument.starts_with("fact_") {
                    MemoryReadOptions {
                        fact_id: Some(argument.into()),
                        ..Default::default()
                    }
                } else if argument.starts_with("mem_") {
                    MemoryReadOptions {
                        entry_id: Some(argument.into()),
                        ..Default::default()
                    }
                } else {
                    return Err(failure(
                        "invalid_args",
                        "Use show with a stable entry or fact ID",
                    ));
                };
                serde_json::to_value(self.store.read_page_cancellable(
                    options,
                    MemoryQueryLimits::default(),
                    cancel,
                )?)
                .map_err(|_| failure("memory_output", "Memory read result cannot be encoded"))?
            }
            "search" => {
                let options = if argument.starts_with('{') {
                    serde_json::from_str::<MemorySearchOptions>(argument)
                        .map_err(|_| failure("invalid_args", "Memory search options are invalid"))?
                } else {
                    MemorySearchOptions {
                        query: argument.into(),
                        ..Default::default()
                    }
                };
                serde_json::to_value(self.store.search_cancellable(
                    options,
                    MemoryQueryLimits::default(),
                    cancel,
                )?)
                .map_err(|_| failure("memory_output", "Memory search result cannot be encoded"))?
            }
            "sources" => {
                let snapshot = self.store.snapshot_cancellable(cancel)?;
                let entry = snapshot
                    .entries
                    .iter()
                    .find(|entry| {
                        entry.id == argument || entry.facts.iter().any(|fact| fact.id == argument)
                    })
                    .ok_or_else(|| {
                        failure("memory_not_found", "Select a visible entry or fact ID")
                    })?;
                let refs = entry
                    .facts
                    .iter()
                    .filter(|fact| argument == entry.id || argument == fact.id)
                    .flat_map(|fact| &fact.sources)
                    .collect::<Vec<_>>();
                json!({"revision":snapshot.revision,"entry_id":entry.id,"scope":entry.scope,"resources":entry.resources,
                    "sources":snapshot.sources.iter().filter(|source|refs.iter().any(|reference|reference.source_id==source.id && reference.revision==source.revision)).collect::<Vec<_>>()})
            }
            "history" => {
                let versions = self.store.history()?;
                let (start, end, page) = page(versions.len(), argument)?;
                json!({"versions":&versions[start..end],"page":page,"next_page":(end<versions.len()).then_some(page+1),
                    "total":versions.len(),"scope":"retained index versions; current forget constraints survive rollback"})
            }
            "operation" => {
                json!({"receipt":self.store.operation_receipt(argument)?,"current_revision":self.store.snapshot_cancellable(cancel)?.revision})
            }
            _ => {
                return Err(failure(
                    "invalid_args",
                    "Use list, search, show, sources, history or operation",
                ));
            }
        };
        check_cancel(cancel)?;
        self.safe(json!({"settings":self.settings()?,"data":data,"retention":retention()}))
    }
    /// Stage one explicit user action. No filesystem publication occurs here.
    pub fn prepare(
        &self,
        command: &str,
        cancel: &AtomicBool,
    ) -> Result<PreparedMemoryAction, BoundaryError> {
        let (verb, argument) = command
            .trim()
            .split_once(' ')
            .unwrap_or((command.trim(), ""));
        let argument = argument.trim();
        self.writable(verb == "export")?;
        check_cancel(cancel)?;
        let snapshot = self.store.snapshot_cancellable(cancel)?;
        let operation_id = format!("op_{}", RunId::new().0.simple());
        let expected_revision = snapshot.revision.clone();
        let action = match verb {
            "export" => {
                let selected = file::selected(Path::new(argument), &self.config, &self.secrets)?;
                let target = crate::export::prepare_target(&selected)?;
                let _ = file::selected(target.path(), &self.config, &self.secrets)?;
                let backup = self.store.export_backup()?;
                if backup.source_revision != expected_revision {
                    return Err(failure(
                        "memory_conflict",
                        "Memory changed while preparing export; review a fresh backup",
                    ));
                }
                let bytes = serde_json::to_vec(&backup)
                    .map_err(|_| failure("memory_output", "Memory backup cannot be encoded"))?;
                if bytes.len() > MAX_INPUT_BYTES {
                    return Err(failure(
                        "memory_limit",
                        "Memory backup exceeds its byte limit",
                    ));
                }
                let review=self.safe(json!({"operation":"export","target":target.path(),"source_revision":backup.source_revision,"forget_revision":backup.forget_revision,"checksum":backup.checksum,"bytes":bytes.len(),"coverage":backup.coverage,"retention":retention()}))?;
                check_cancel(cancel)?;
                return Ok(PreparedMemoryAction {
                    root: self.store.root().into(),
                    workspace: self.store.workspace().clone(),
                    expected_revision,
                    operation_id,
                    review,
                    action: PreparedAction::Export {
                        target: Box::new(target),
                        bytes,
                    },
                });
            }
            _ => {
                let change = match verb {
                    "diff" | "review" => {
                        serde_json::from_str::<MemoryChange>(argument).map_err(|_| {
                            failure(
                                "memory_format",
                                "Use diff followed by one MemoryChange JSON object",
                            )
                        })?
                    }
                    "forget" => {
                        let entry = snapshot
                            .entries
                            .iter()
                            .find(|entry| entry.facts.iter().any(|fact| fact.id == argument))
                            .ok_or_else(|| {
                                failure("memory_not_found", "Select a visible fact ID to forget")
                            })?;
                        MemoryChange::Forget {
                            scope: entry.scope.clone(),
                            fact_ids: vec![argument.into()],
                        }
                    }
                    "correct" => {
                        let (id, text) = argument
                            .split_once(' ')
                            .filter(|(_, text)| !text.trim().is_empty())
                            .ok_or_else(|| {
                                failure("invalid_args", "Use correct FACT_ID replacement text")
                            })?;
                        let mut entry = snapshot
                            .entries
                            .iter()
                            .find(|entry| entry.facts.iter().any(|fact| fact.id == id))
                            .cloned()
                            .ok_or_else(|| {
                                failure("memory_not_found", "Select a visible fact ID to correct")
                            })?;
                        let revision = self.store.administrative_request_digest(
                            &json!({"text":text,"operation":operation_id}),
                        )?;
                        let source = MemorySource {
                            id: new_source_id(),
                            kind: MemorySourceKind::User,
                            scope: entry.scope.clone(),
                            reference: format!("tui:{operation_id}"),
                            revision: revision.clone(),
                            observed_at: format!(
                                "unix:{}",
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map_err(|_| failure(
                                        "clock_error",
                                        "Current time is unavailable"
                                    ))?
                                    .as_secs()
                            ),
                            description: "Explicit user correction in the memory manager".into(),
                            metadata_withheld: false,
                        };
                        if let Some(fact) = entry.facts.iter_mut().find(|fact| fact.id == id) {
                            fact.text = text.into();
                            fact.kind = MemoryKind::UserCorrection;
                            fact.sources = vec![MemorySourceRef {
                                source_id: source.id.clone(),
                                revision,
                            }];
                            fact.aliases.clear();
                            fact.derived_from.clear();
                        }
                        entry.updated_at = source.observed_at.clone();
                        MemoryChange::Upsert {
                            entry,
                            sources: vec![source],
                        }
                    }
                    "rollback" => MemoryChange::RestoreVersion {
                        version: argument.into(),
                    },
                    "restore" => MemoryChange::RestoreDocument {
                        backup: self.store.parse_backup(&file::read(
                            Path::new(argument),
                            &self.config,
                            &self.secrets,
                            cancel,
                        )?)?,
                    },
                    _ => {
                        return Err(failure(
                            "invalid_args",
                            "Use correct, forget, diff, rollback, export or restore",
                        ));
                    }
                };
                let request=self.store.administrative_request_digest(&json!({"interface":"explicit_user_memory_manager","expected_revision":expected_revision,"operation_id":operation_id,"change":change}))?;
                PreparedAction::Change(Box::new(self.store.propose_host_request(
                    &expected_revision,
                    &operation_id,
                    &request,
                    change,
                )?))
            }
        };
        let review = match &action {
            PreparedAction::Change(proposal) => {
                let (before, after) = changed_entries(proposal);
                self.safe(json!({"operation_id":operation_id,"expected_revision":expected_revision,"request_digest":proposal.host_request_digest(),"proposed_digest":proposal.proposed_document_digest,
                    "change":change_summary(proposal.change()),"before":before,"after":after,
                    "source_changes":changed_sources(&snapshot,proposal),
                    "diff_coverage":"complete changed facts, source and entry metadata, and resource links; unchanged facts are omitted","retention":retention()}))?
            }
            PreparedAction::Export { .. } => {
                return Err(failure("memory_state", "Export review was not prepared"));
            }
        };
        check_cancel(cancel)?;
        Ok(PreparedMemoryAction {
            root: self.store.root().into(),
            workspace: self.store.workspace().clone(),
            expected_revision,
            operation_id,
            review,
            action,
        })
    }
    /// Apply exactly the reviewed action once. The caller must obtain explicit
    /// user confirmation and must serialize this operation against active Runs.
    pub fn confirm(
        &self,
        prepared: PreparedMemoryAction,
        cancel: &AtomicBool,
    ) -> Result<Value, BoundaryError> {
        self.writable(matches!(&prepared.action, PreparedAction::Export { .. }))?;
        if prepared.root != self.store.root() || prepared.workspace != *self.store.workspace() {
            return Err(failure(
                "memory_scope",
                "Memory review belongs to another store or workspace",
            ));
        }
        check_cancel(cancel)?;
        if self.store.snapshot_cancellable(cancel)?.revision != prepared.expected_revision {
            return Err(failure(
                "memory_conflict",
                "Memory changed after review; prepare and review a fresh action",
            ));
        }
        let value = match prepared.action {
            PreparedAction::Change(proposal) => {
                let outcome = self.store.commit_cancellable(&proposal, cancel)?;
                json!({"operation_id":prepared.operation_id,"status":outcome.status,"receipt":outcome.receipt,"revision":outcome.revision,"detail":outcome.error,
                    "requires_recovery":outcome.status==MemoryCommitStatus::Unknown,"input_replayed":false,"projection_refresh_required":outcome.projection_refresh_required,"historical_copies_retained":outcome.historical_copies_retained,"retention":retention()})
            }
            PreparedAction::Export { target, bytes } => {
                let _ = file::selected(target.path(), &self.config, &self.secrets)?;
                let _ = file::selected(target.requested_path(), &self.config, &self.secrets)?;
                if saya_core::redaction::contains_secret(
                    std::str::from_utf8(&bytes).map_err(|_| {
                        failure("memory_format", "Memory backup encoding is invalid")
                    })?,
                    &self.secrets.snapshot()?,
                ) {
                    return Err(failure(
                        "sensitive_data",
                        "Memory export contains protected data",
                    ));
                }
                json!({"operation_id":prepared.operation_id,"publication":crate::export::publish_prepared(*target,&bytes,&self.config.cwd,self.config.mode,&Cancel(cancel))?,"source_revision":prepared.expected_revision,"retention":retention()})
            }
        };
        self.safe(value)
    }
}
struct Cancel<'a>(&'a AtomicBool);
impl Cancellation for Cancel<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}
fn page(total: usize, argument: &str) -> Result<(usize, usize, usize), BoundaryError> {
    let page = if argument.is_empty() {
        1
    } else {
        argument
            .parse::<usize>()
            .map_err(|_| failure("invalid_args", "Page must be a positive number"))?
    };
    let start = page
        .checked_sub(1)
        .and_then(|page| page.checked_mul(PAGE_ITEMS))
        .ok_or_else(|| failure("invalid_args", "Page exceeds its bounds"))?;
    if (total == 0 && page != 1) || (total > 0 && start >= total) {
        return Err(failure("invalid_args", "Page is outside this result set"));
    }
    Ok((start, start.saturating_add(PAGE_ITEMS).min(total), page))
}
fn retention() -> Value {
    json!({"active_view":"global and current workspace","topic_bodies":"reference only; retained with their owner","historical_versions":"retained; current forgetting constraints survive rollback","original_session_evidence":"retained with source owner","existing_exports":"retained with export owner"})
}

fn changed_entries(
    proposal: &MemoryProposal,
) -> (
    Vec<saya_core::global_memory::MemoryEntry>,
    Vec<saya_core::global_memory::MemoryEntry>,
) {
    let old = proposal
        .before_entries
        .iter()
        .map(|entry| (entry.id.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    let new = proposal
        .after_entries
        .iter()
        .map(|entry| (entry.id.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    let ids = old
        .keys()
        .chain(new.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    let mut before = Vec::new();
    let mut after = Vec::new();
    for id in ids {
        if old.get(id) == new.get(id) {
            continue;
        }
        let old = old.get(id).copied();
        let new = new.get(id).copied();
        let old_facts = old
            .into_iter()
            .flat_map(|entry| &entry.facts)
            .map(|fact| (fact.id.as_str(), fact))
            .collect::<BTreeMap<_, _>>();
        let new_facts = new
            .into_iter()
            .flat_map(|entry| &entry.facts)
            .map(|fact| (fact.id.as_str(), fact))
            .collect::<BTreeMap<_, _>>();
        let changed = old_facts
            .keys()
            .chain(new_facts.keys())
            .copied()
            .filter(|id| old_facts.get(id) != new_facts.get(id))
            .collect::<BTreeSet<_>>();
        if let Some(entry) = old {
            let mut entry = entry.clone();
            entry
                .facts
                .retain(|fact| changed.contains(fact.id.as_str()));
            before.push(entry);
        }
        if let Some(entry) = new {
            let mut entry = entry.clone();
            entry
                .facts
                .retain(|fact| changed.contains(fact.id.as_str()));
            after.push(entry);
        }
    }
    (before, after)
}
fn change_summary(change: &MemoryChange) -> Value {
    match change {
        MemoryChange::Upsert { entry, sources } => {
            json!({"operation":"upsert","entry_id":entry.id,"sources":sources})
        }
        MemoryChange::Forget { scope, fact_ids } => {
            json!({"operation":"forget","scope":scope,"fact_ids":fact_ids})
        }
        MemoryChange::RetractSource {
            source_id,
            revision,
        } => json!({"operation":"retract_source","source_id":source_id,"revision":revision}),
        MemoryChange::RestoreVersion { version } => {
            json!({"operation":"restore_version","version":version})
        }
        MemoryChange::RestoreDocument { backup } => {
            json!({"operation":"restore_document","checksum":backup.checksum,"source_revision":backup.source_revision,"forget_revision":backup.forget_revision,"coverage":backup.coverage,"topic_references":backup.resources})
        }
    }
}

fn changed_sources(
    snapshot: &saya_core::global_memory::MemorySnapshot,
    proposal: &MemoryProposal,
) -> Vec<Value> {
    let before = snapshot
        .sources
        .iter()
        .map(|source| (source.id.as_str(), source))
        .collect::<BTreeMap<_, _>>();
    let after = proposal
        .document_for_host()
        .sources
        .iter()
        .filter(|source| source.scope.visible_in(&snapshot.workspace))
        .map(|source| (source.id.as_str(), source))
        .collect::<BTreeMap<_, _>>();
    before
        .keys()
        .chain(after.keys())
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|id| before.get(id) != after.get(id))
        .map(|id| json!({"source_id":id,"before":before.get(id),"after":after.get(id)}))
        .collect()
}
