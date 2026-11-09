//! Host-bound memory operations prepared for the runtime's authorization flow.
//!
//! This component never persists session intent or grants itself authority.
//! The runtime records intent/result around execute_prepared and supplies a
//! current, invocation-bound gate for every call, including idempotent retries.
use crate::{
    boundary::{ToolContext, ToolDefinition},
    contract::{
        BoundaryError, ErrorInfo, PermissionMode, RunId, Session, SessionId, ToolCall, ToolResult,
        WorkMode,
    },
    global_memory::{
        GlobalMemoryStore, MemoryChange, MemoryCommitStatus, MemoryKind, MemoryOperationReceipt,
        MemoryProposal, MemoryQueryLimits, MemoryScope,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

mod args;
mod evidence;
mod read;
mod review;
#[cfg(test)]
mod tests;

pub use evidence::{
    EvidenceSelector, EvidenceState, MemoryEvidence, MemoryEvidenceCatalog,
    MemoryEvidenceCatalogItem, MemoryEvidenceCursor, MemoryEvidencePage,
};
pub use review::{MemoryEntryReview, MemoryFactReview, MemoryReviewMetadata};

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
fn cancelled(cancel: &AtomicBool) -> Result<(), BoundaryError> {
    if cancel.load(Ordering::Acquire) {
        return Err(failure("cancelled", "Memory tool was cancelled"));
    }
    Ok(())
}
fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn valid_id(value: &str, prefix: &str) -> bool {
    value.starts_with(prefix)
        && value.len() > prefix.len()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn encode<T: Serialize + ?Sized>(value: &T, max: usize) -> Result<Vec<u8>, BoundaryError> {
    struct Buffer {
        bytes: Vec<u8>,
        max: usize,
    }
    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.bytes.len().saturating_add(bytes.len()) > self.max {
                return Err(std::io::Error::other("memory tool byte limit"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = Buffer {
        bytes: Vec::new(),
        max,
    };
    serde_json::to_writer(&mut output, value).map_err(|_| {
        failure(
            "memory_tool_limit",
            "Memory tool input exceeds its encoded byte limit",
        )
    })?;
    Ok(output.bytes)
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ManagedMemoryLimits {
    pub max_argument_bytes: usize,
    pub max_evidence_bytes: usize,
    pub max_history_bytes: usize,
    pub max_evidence_excerpt_bytes: usize,
    pub max_review_bytes: usize,
    pub max_result_bytes: usize,
    pub max_changed_facts: usize,
    pub max_evidence_per_fact: usize,
    pub max_total_evidence: usize,
}
impl Default for ManagedMemoryLimits {
    fn default() -> Self {
        Self {
            max_argument_bytes: 64 * 1024,
            max_evidence_bytes: 1024 * 1024,
            max_history_bytes: 32 * 1024 * 1024,
            max_evidence_excerpt_bytes: 2048,
            max_review_bytes: 32 * 1024,
            max_result_bytes: 64 * 1024,
            max_changed_facts: 32,
            max_evidence_per_fact: 8,
            max_total_evidence: 32,
        }
    }
}
impl ManagedMemoryLimits {
    pub fn validate(&self) -> Result<(), BoundaryError> {
        if self.max_argument_bytes == 0
            || self.max_argument_bytes > 64 * 1024
            || self.max_evidence_bytes == 0
            || self.max_evidence_bytes > 4 * 1024 * 1024
            || self.max_history_bytes == 0
            || self.max_history_bytes > 128 * 1024 * 1024
            || self.max_evidence_excerpt_bytes > 16 * 1024
            || self.max_review_bytes < 1024
            || self.max_review_bytes > 64 * 1024
            || self.max_result_bytes < 1024
            || self.max_result_bytes > 128 * 1024
            || self.max_changed_facts == 0
            || self.max_changed_facts > 64
            || self.max_evidence_per_fact == 0
            || self.max_evidence_per_fact > 32
            || self.max_total_evidence == 0
            || self.max_total_evidence > 128
        {
            return Err(failure(
                "memory_config",
                "Managed memory tool limits are invalid",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryToolEffect {
    Read,
    Mutation,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryAuthorizationSource {
    Rule,
    IndependentModelReview,
    User,
    Bypass,
}
#[derive(Debug, Clone)]
pub struct MemoryAuthorization {
    pub binding_fingerprint: String,
    pub source: MemoryAuthorizationSource,
}
#[derive(Debug, Clone, Serialize)]
pub struct MemoryAuthorizationBinding {
    pub fingerprint: String,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub call_id: String,
    pub tool: String,
    pub effect: MemoryToolEffect,
    pub work_mode: WorkMode,
    pub permissions: PermissionMode,
    pub workspace_identity: String,
    pub root_digest: String,
    pub arguments_digest: String,
    pub history_digest: Option<String>,
    pub evidence_digest: String,
    pub expected_revision: Option<String>,
    pub host_request_digest: Option<String>,
    pub proposal_digest: Option<String>,
    pub review_digest: String,
}

/// The runtime checks its rules, current mode/revisions and this exact binding.
/// Auto mutations require independent review or current human fallback. Manual
/// approval requires a current user grant for every invocation. The second
/// check revalidates the same dispatch lease immediately before store access.
pub trait MemoryGate: Send + Sync {
    fn authorize(
        &self,
        binding: &MemoryAuthorizationBinding,
        context: &ToolContext,
    ) -> Result<MemoryAuthorization, BoundaryError>;
    fn finish(&self) -> Result<(), BoundaryError> {
        Ok(())
    }
}
struct Lease<'a>(&'a dyn MemoryGate);
impl Drop for Lease<'_> {
    fn drop(&mut self) {
        let _ = self.0.finish();
    }
}

enum PreparedAction {
    Read(args::ReadArgs),
    Search(crate::global_memory::MemorySearchOptions),
    Evidence(args::EvidenceArgs),
    Mutation(Box<MemoryProposal>),
    AlreadyCommitted(MemoryOperationReceipt),
}

/// An owned, non-cloneable preparation. The root, selected evidence and proposed
/// data remain host-owned. Passing it by value prevents a second execution.
pub struct PreparedMemoryCall {
    call: ToolCall,
    binding: MemoryAuthorizationBinding,
    review: Value,
    review_truncated: bool,
    redaction_probes: Vec<evidence::RedactionProbe>,
    action: PreparedAction,
}
impl PreparedMemoryCall {
    pub fn call(&self) -> &ToolCall {
        &self.call
    }
    pub fn binding(&self) -> &MemoryAuthorizationBinding {
        &self.binding
    }
    pub fn review_truncated(&self) -> bool {
        self.review_truncated
    }
}

pub struct ManagedMemoryTools {
    store: Arc<GlobalMemoryStore>,
    query_limits: MemoryQueryLimits,
    limits: ManagedMemoryLimits,
    gate: Arc<dyn MemoryGate>,
}
impl ManagedMemoryTools {
    pub fn new(
        store: Arc<GlobalMemoryStore>,
        query_limits: MemoryQueryLimits,
        limits: ManagedMemoryLimits,
        gate: Arc<dyn MemoryGate>,
    ) -> Result<Self, BoundaryError> {
        query_limits.validate()?;
        limits.validate()?;
        Ok(Self {
            store,
            query_limits,
            limits,
            gate,
        })
    }
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        args::definitions(self.store.limits(), self.query_limits, self.limits)
    }
    pub fn evidence_catalog(
        &self,
        session: &Session,
        context: &ToolContext,
        cancel: &AtomicBool,
    ) -> Result<MemoryEvidenceCatalog, BoundaryError> {
        self.validate_context(session, context)?;
        evidence::catalog(self, session, cancel)
    }
    /// Re-redacts immediately before display/review. A credential rotation that
    /// changes review content invalidates the old binding instead of returning
    /// a differently bound review silently.
    pub fn review_metadata(&self, prepared: &PreparedMemoryCall) -> Result<Value, BoundaryError> {
        for probe in &prepared.redaction_probes {
            probe.check(self)?;
        }
        let current = self.store.redact_review_value(prepared.review.clone())?;
        if digest(&encode(&current, self.limits.max_review_bytes)?)
            != prepared.binding.review_digest
        {
            return Err(failure(
                "stale_approval",
                "Memory review data changed before authorization",
            ));
        }
        Ok(current)
    }
    /// Prepare review data from a host-verified persisted session. Reading a
    /// model query is deferred until execution. Mutations inspect current
    /// memory to compute their proposed, bounded host review.
    pub fn prepare(
        &self,
        call: &ToolCall,
        session: &Session,
        context: &ToolContext,
        cancel: &AtomicBool,
    ) -> Result<PreparedMemoryCall, BoundaryError> {
        cancelled(cancel)?;
        self.validate_context(session, context)?;
        let parsed = args::parse(call, self.store.limits(), self.query_limits, self.limits)?;
        if parsed.effect() == MemoryToolEffect::Mutation && context.run.work_mode == WorkMode::Plan
        {
            return Err(failure(
                "mode_denied",
                "PLAN permits read-only memory operations",
            ));
        }
        let normalized = serde_json::to_value(&parsed)
            .map_err(|_| failure("invalid_args", "Memory arguments cannot be normalized"))?;
        let (safe, arguments_probe) = evidence::probe(self, normalized.clone())?;
        if safe != normalized {
            return Err(failure(
                "sensitive_data",
                "Memory arguments contain protected data",
            ));
        }
        let call_identity = serde_json::json!({"call_id":call.call_id,"tool":call.name});
        if self.store.redact_review_value(call_identity.clone())? != call_identity {
            return Err(failure(
                "sensitive_data",
                "Memory invocation identity contains protected data",
            ));
        }
        let arguments_digest = digest(&encode(&parsed, self.limits.max_argument_bytes)?);
        let mut bundle = None;
        let mut scope = None;
        let mut host_request_digest = None;
        let mut tool_evidence_kept_in_workspace = false;
        let action = match &parsed {
            args::Parsed::Read(args) => PreparedAction::Read(args.clone()),
            args::Parsed::Search(args) => PreparedAction::Search(args.options(self.query_limits)),
            args::Parsed::Evidence(args) => PreparedAction::Evidence(args.clone()),
            args::Parsed::Update(args) => {
                let evidence = evidence::selected(
                    self,
                    session,
                    args.facts
                        .iter()
                        .flat_map(|fact| fact.evidence.iter().cloned()),
                    cancel,
                )?;
                let selected_scope = args.scope.resolve(&self.store);
                let request = self.request_digest(session, &parsed, &evidence)?;
                let action =
                    if let Some(receipt) = self.retry_receipt(&args.operation_id, &request)? {
                        PreparedAction::AlreadyCommitted(receipt)
                    } else {
                        let change = self.update_change(args, session, &evidence)?;
                        PreparedAction::Mutation(Box::new(self.store.propose_host_request(
                            &args.expected_revision,
                            &args.operation_id,
                            &request,
                            change,
                        )?))
                    };
                tool_evidence_kept_in_workspace = selected_scope == MemoryScope::Global
                    && evidence
                        .selected
                        .values()
                        .any(|item| item.state != EvidenceState::UserInput);
                scope = Some(selected_scope);
                host_request_digest = Some(request);
                bundle = Some(evidence);
                action
            }
            args::Parsed::Forget(args) => {
                let evidence = evidence::selected(self, session, std::iter::empty(), cancel)?;
                let selected_scope = args.scope.resolve(&self.store);
                let request = self.request_digest(session, &parsed, &evidence)?;
                let action =
                    if let Some(receipt) = self.retry_receipt(&args.operation_id, &request)? {
                        PreparedAction::AlreadyCommitted(receipt)
                    } else {
                        PreparedAction::Mutation(Box::new(self.store.propose_host_request(
                            &args.expected_revision,
                            &args.operation_id,
                            &request,
                            MemoryChange::Forget {
                                scope: selected_scope.clone(),
                                fact_ids: args.fact_ids.clone(),
                            },
                        )?))
                    };
                scope = Some(selected_scope);
                host_request_digest = Some(request);
                bundle = Some(evidence);
                action
            }
        };
        let proposal = match &action {
            PreparedAction::Mutation(proposal) => Some(proposal.as_ref()),
            _ => None,
        };
        let receipt = match &action {
            PreparedAction::AlreadyCommitted(receipt) => Some(receipt),
            _ => None,
        };
        let (review, review_truncated, review_probe) = review::build(
            self,
            call,
            review::ReviewInput {
                parsed: &parsed,
                arguments_digest: &arguments_digest,
                host_request_digest: host_request_digest.as_deref(),
                scope,
                bundle: bundle.as_ref(),
                proposal,
                receipt,
                tool_evidence_kept_in_workspace,
            },
        )?;
        let evidence_digest = bundle
            .as_ref()
            .map(|bundle| bundle.digest(self.limits))
            .transpose()?
            .unwrap_or_else(|| digest(b"[]"));
        let history_digest = if matches!(parsed, args::Parsed::Evidence(_)) {
            Some(evidence::hash(
                &session.history,
                self.limits.max_history_bytes,
                cancel,
            )?)
        } else {
            bundle.as_ref().map(|bundle| bundle.history_digest.clone())
        };
        let proposal_digest = proposal
            .map(|proposal| proposal.proposed_document_digest.clone())
            .or_else(|| receipt.map(|receipt| receipt.document_digest.clone()));
        let mut probes = vec![arguments_probe, review_probe];
        if let Some(bundle) = bundle {
            probes.extend(bundle.probes);
        }
        let mut binding = MemoryAuthorizationBinding {
            fingerprint: String::new(),
            session_id: session.id,
            run_id: context.run.id,
            call_id: call.call_id.clone(),
            tool: call.name.clone(),
            effect: parsed.effect(),
            work_mode: context.run.work_mode,
            permissions: context.run.permissions,
            workspace_identity: self.store.workspace().id.clone(),
            root_digest: digest(self.store.root().to_string_lossy().as_bytes()),
            arguments_digest,
            history_digest,
            evidence_digest,
            expected_revision: parsed.expected_revision().map(str::to_owned),
            host_request_digest,
            proposal_digest,
            review_digest: digest(&encode(&review, self.limits.max_review_bytes)?),
        };
        binding.fingerprint = digest(&encode(
            &("saya-managed-memory-v1", &binding),
            self.limits.max_review_bytes,
        )?);
        cancelled(cancel)?;
        Ok(PreparedMemoryCall {
            call: call.clone(),
            binding,
            review,
            review_truncated,
            redaction_probes: probes,
            action,
        })
    }
    fn request_digest(
        &self,
        session: &Session,
        parsed: &args::Parsed,
        bundle: &evidence::Bundle,
    ) -> Result<String, BoundaryError> {
        Ok(digest(&encode(
            &(
                "saya-memory-host-request-v1",
                session.id,
                self.store.workspace(),
                parsed,
                bundle.digest(self.limits)?,
            ),
            self.limits.max_argument_bytes,
        )?))
    }
    fn retry_receipt(
        &self,
        operation_id: &str,
        request_digest: &str,
    ) -> Result<Option<MemoryOperationReceipt>, BoundaryError> {
        let receipt = self.store.operation_receipt(operation_id)?;
        if receipt
            .as_ref()
            .is_some_and(|receipt| receipt.host_request_digest.as_deref() != Some(request_digest))
        {
            return Err(failure(
                "memory_idempotency_conflict",
                "Memory operation identity belongs to another normalized request",
            ));
        }
        Ok(receipt)
    }
    fn update_change(
        &self,
        args: &args::UpdateArgs,
        session: &Session,
        bundle: &evidence::Bundle,
    ) -> Result<MemoryChange, BoundaryError> {
        use crate::global_memory::{MemoryEntry, MemoryFact, MemorySourceRef};
        let snapshot = self.store.snapshot()?;
        if snapshot.revision != args.expected_revision {
            return Err(failure(
                "memory_conflict",
                "Memory changed before update preparation",
            ));
        }
        let scope = args.scope.resolve(&self.store);
        let entry_id = args.entry_id.clone().unwrap_or_else(|| {
            format!(
                "mem_{}",
                digest(format!("{}:{}:entry", session.id.0, args.operation_id).as_bytes())
            )
        });
        let mut entry = match snapshot.entries.iter().find(|entry| entry.id == entry_id) {
            Some(entry) if entry.scope == scope => entry.clone(),
            Some(_) => {
                return Err(failure(
                    "memory_scope",
                    "Memory entry scope cannot be reassigned",
                ));
            }
            None if args.entry_id.is_some() => {
                return Err(failure(
                    "memory_not_found",
                    "Memory update entry is not currently visible",
                ));
            }
            None => MemoryEntry {
                id: entry_id,
                title: args.title.clone().ok_or_else(|| {
                    failure("invalid_args", "A new memory entry requires a title")
                })?,
                scope: scope.clone(),
                updated_at: "not_recorded".into(),
                facts: Vec::new(),
                resources: Vec::new(),
            },
        };
        if let Some(title) = &args.title {
            entry.title = title.clone();
        }
        entry.updated_at = "not_recorded".into();
        let mut sources = std::collections::BTreeMap::new();
        for (index, fact) in args.facts.iter().enumerate() {
            let selections = fact
                .evidence
                .iter()
                .collect::<std::collections::BTreeSet<_>>();
            if selections.len() != fact.evidence.len() {
                return Err(failure(
                    "memory_evidence_invalid",
                    "Memory fact repeats an evidence selector",
                ));
            }
            let selected = fact
                .evidence
                .iter()
                .map(|selector| {
                    bundle.selected.get(selector).ok_or_else(|| {
                        failure(
                            "memory_evidence_invalid",
                            "Memory fact evidence was not verified",
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let has_user = selected
                .iter()
                .any(|source| source.state == EvidenceState::UserInput);
            if (scope == MemoryScope::Global
                || matches!(
                    fact.kind,
                    MemoryKind::ExplicitPreference
                        | MemoryKind::UserCorrection
                        | MemoryKind::TaskDecision
                ))
                && !has_user
            {
                return Err(failure(
                    "memory_evidence_invalid",
                    "Global facts and user preferences or decisions require actual user-message evidence",
                ));
            }
            let mut references = Vec::new();
            for evidence in selected {
                if scope == MemoryScope::Global && evidence.state != EvidenceState::UserInput {
                    continue;
                }
                let source = evidence::source(self, evidence, &scope)?;
                references.push(MemorySourceRef {
                    source_id: source.id.clone(),
                    revision: source.revision.clone(),
                });
                sources.insert(source.id.clone(), source);
            }
            let id = fact.id.clone().unwrap_or_else(|| {
                format!(
                    "fact_{}",
                    digest(
                        format!("{}:{}:fact:{index}", session.id.0, args.operation_id).as_bytes()
                    )
                )
            });
            let replacement = MemoryFact {
                id: id.clone(),
                kind: fact.kind,
                text: fact.text.clone(),
                sources: references,
                aliases: fact.aliases.clone(),
                derived_from: fact.derived_from.clone(),
            };
            if let Some(existing) = entry.facts.iter_mut().find(|fact| fact.id == id) {
                *existing = replacement;
            } else {
                entry.facts.push(replacement);
            }
        }
        Ok(MemoryChange::Upsert {
            entry,
            sources: sources.into_values().collect(),
        })
    }
    fn authorize(
        &self,
        prepared: &PreparedMemoryCall,
        context: &ToolContext,
    ) -> Result<(), BoundaryError> {
        let authorization = self.gate.authorize(&prepared.binding, context)?;
        if authorization.binding_fingerprint != prepared.binding.fingerprint {
            return Err(failure(
                "stale_approval",
                "Memory authorization belongs to another preparation",
            ));
        }
        let allowed = match context.run.permissions {
            PermissionMode::Manual => authorization.source == MemoryAuthorizationSource::User,
            PermissionMode::Bypass => matches!(
                authorization.source,
                MemoryAuthorizationSource::Bypass | MemoryAuthorizationSource::User
            ),
            PermissionMode::Auto if prepared.binding.effect == MemoryToolEffect::Mutation => {
                matches!(
                    authorization.source,
                    MemoryAuthorizationSource::IndependentModelReview
                        | MemoryAuthorizationSource::User
                )
            }
            PermissionMode::Auto => authorization.source != MemoryAuthorizationSource::Bypass,
        };
        if !allowed {
            return Err(failure(
                "needs_confirmation",
                "Memory invocation requires a current authorization with sufficient review evidence",
            ));
        }
        Ok(())
    }
    /// Consumes one preparation. The caller persists intent before this call
    /// and saves the returned result or BoundaryError.2 without replaying it.
    pub fn execute_prepared(
        &self,
        prepared: PreparedMemoryCall,
        session: &Session,
        context: &ToolContext,
        cancel: &AtomicBool,
    ) -> Result<ToolResult, BoundaryError> {
        cancelled(cancel)?;
        self.validate_context(session, context)?;
        if prepared.binding.effect == MemoryToolEffect::Mutation
            && context.run.work_mode == WorkMode::Plan
        {
            return Err(failure(
                "mode_denied",
                "PLAN permits read-only memory operations",
            ));
        }
        if prepared.binding.session_id != session.id
            || prepared.binding.run_id != context.run.id
            || prepared.binding.work_mode != context.run.work_mode
            || prepared.binding.permissions != context.run.permissions
            || prepared.binding.root_digest
                != digest(self.store.root().to_string_lossy().as_bytes())
        {
            return Err(failure(
                "stale_approval",
                "Memory execution context changed after preparation",
            ));
        }
        if let Some(expected) = &prepared.binding.history_digest
            && evidence::hash(&session.history, self.limits.max_history_bytes, cancel)? != *expected
        {
            return Err(failure(
                "stale_approval",
                "Persisted evidence changed after memory preparation",
            ));
        }
        let _ = self.review_metadata(&prepared)?;
        let _lease = Lease(self.gate.as_ref());
        self.authorize(&prepared, context)?;
        cancelled(cancel)?;
        self.authorize(&prepared, context)?;
        let _ = self.review_metadata(&prepared)?;
        cancelled(cancel)?;
        match prepared.action {
            PreparedAction::Read(args) => self.execute_read(args, cancel),
            PreparedAction::Search(options) => {
                let page = self
                    .store
                    .search_cancellable(options, self.query_limits, cancel)?;
                self.query_result(
                    serde_json::to_value(&page).map_err(|_| {
                        failure(
                            "serialization_error",
                            "Memory search result cannot be encoded",
                        )
                    })?,
                    page.truncated,
                )
            }
            PreparedAction::Evidence(args) => {
                let page = evidence::page(self, session, &args, cancel)?;
                let truncated = page.truncated;
                self.query_result(
                    serde_json::to_value(page).map_err(|_| {
                        failure(
                            "serialization_error",
                            "Memory evidence page cannot be encoded",
                        )
                    })?,
                    truncated,
                )
            }
            PreparedAction::Mutation(proposal) => {
                let summary = mutation_summary(&proposal);
                let outcome = self.store.commit_cancellable(&proposal, cancel)?;
                self.mutation_result(outcome, summary)
            }
            PreparedAction::AlreadyCommitted(receipt) => {
                let current = self
                    .retry_receipt(
                        &receipt.operation_id,
                        receipt.host_request_digest.as_deref().ok_or_else(|| {
                            failure(
                                "memory_idempotency_conflict",
                                "Legacy memory operation has no host request binding",
                            )
                        })?,
                    )?
                    .ok_or_else(|| {
                        failure(
                            "memory_idempotency_conflict",
                            "Recorded memory operation disappeared",
                        )
                    })?;
                let snapshot = self.store.snapshot_cancellable(cancel)?;
                let history = self.store.history()?;
                let outcome = crate::global_memory::MemoryCommitOutcome {
                    status: MemoryCommitStatus::AlreadyCommitted,
                    receipt: current,
                    revision: Some(snapshot.revision),
                    error: None,
                    historical_copies_retained: !history.is_empty(),
                    projection_refresh_required: true,
                };
                self.mutation_result(outcome, MutationSummary::default())
            }
        }
    }
    fn query_result(&self, value: Value, truncated: bool) -> Result<ToolResult, BoundaryError> {
        if self.store.redact_review_value(value.clone())? != value {
            return Err(failure(
                "sensitive_data",
                "Memory query result changed under current credential protection",
            ));
        }
        let result = ToolResult {
            ok: true,
            data: Some(value),
            error: None,
            truncated,
        };
        let _ = encode(&result, self.limits.max_result_bytes)?;
        Ok(result)
    }
    fn mutation_result(
        &self,
        outcome: crate::global_memory::MemoryCommitOutcome,
        summary: MutationSummary,
    ) -> Result<ToolResult, BoundaryError> {
        let omitted = summary
            .fact_ids
            .len()
            .saturating_sub(self.limits.max_changed_facts);
        let entries_omitted = summary
            .entry_ids
            .len()
            .saturating_sub(self.limits.max_changed_facts);
        let value = serde_json::json!({"status":outcome.status,"operation_id":outcome.receipt.operation_id,
            "recorded_generation":outcome.receipt.generation,"recorded_document_digest":outcome.receipt.document_digest,
            "change_digest":outcome.receipt.change_digest,"host_request_digest":outcome.receipt.host_request_digest,
            "current_revision":outcome.revision,"applied_now":outcome.status==MemoryCommitStatus::Committed,
            "content_state":"Read current memory for current contents",
            "affected_fact_ids":summary.fact_ids.into_iter().take(self.limits.max_changed_facts).collect::<Vec<_>>(),"affected_fact_ids_omitted":omitted,
            "affected_entry_ids":summary.entry_ids.into_iter().take(self.limits.max_changed_facts).collect::<Vec<_>>(),"affected_entry_ids_omitted":entries_omitted,
            "projection_refresh_required":outcome.projection_refresh_required,"historical_copies_retained":outcome.historical_copies_retained,
            "retained_scopes":["unlinked_topic_files","original_evidence","existing_exports"],"detail":outcome.error});
        let value = self.store.redact_review_value(value)?;
        let unknown = outcome.status == MemoryCommitStatus::Unknown;
        let info = failure(
            "outcome_unknown",
            "Memory publication requires host recovery and was not replayed",
        )
        .0;
        let result = ToolResult {
            ok: !unknown,
            data: Some(value),
            error: unknown.then_some(info.clone()),
            truncated: omitted > 0 || entries_omitted > 0,
        };
        if let Err(error) = encode(&result, self.limits.max_result_bytes) {
            return Err(BoundaryError(
                error.0,
                None,
                Some(Box::new(ToolResult {
                    ok: !unknown,
                    data: Some(
                        serde_json::json!({"status":outcome.status,"projection_refresh_required":true,"result_detail":"Unavailable within result budget"}),
                    ),
                    error: unknown.then_some(info),
                    truncated: true,
                })),
            ));
        }
        if unknown {
            return Err(BoundaryError(info, None, Some(Box::new(result))));
        }
        Ok(result)
    }
    fn validate_context(
        &self,
        session: &Session,
        context: &ToolContext,
    ) -> Result<(), BoundaryError> {
        if session.id != context.run.session_id
            || crate::global_memory::WorkspaceIdentity::from_path(&session.workspace)?
                != *self.store.workspace()
            || crate::global_memory::WorkspaceIdentity::from_path(&context.workspace)?
                != *self.store.workspace()
        {
            return Err(failure(
                "memory_scope",
                "Memory tool context differs from the bound session or workspace",
            ));
        }
        Ok(())
    }
}

#[derive(Default)]
struct MutationSummary {
    fact_ids: Vec<String>,
    entry_ids: Vec<String>,
}
fn mutation_summary(proposal: &MemoryProposal) -> MutationSummary {
    let before = proposal
        .before_entries
        .iter()
        .flat_map(|entry| &entry.facts)
        .map(|fact| (fact.id.as_str(), fact))
        .collect::<std::collections::BTreeMap<_, _>>();
    let after = proposal
        .after_entries
        .iter()
        .flat_map(|entry| &entry.facts)
        .map(|fact| (fact.id.as_str(), fact))
        .collect::<std::collections::BTreeMap<_, _>>();
    let fact_ids = before
        .keys()
        .chain(after.keys())
        .copied()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter(|id| before.get(id) != after.get(id))
        .map(str::to_owned)
        .collect();
    let entry_ids = proposal
        .before_entries
        .iter()
        .chain(&proposal.after_entries)
        .map(|entry| entry.id.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter(|id| {
            proposal.before_entries.iter().find(|entry| entry.id == *id)
                != proposal.after_entries.iter().find(|entry| entry.id == *id)
        })
        .map(str::to_owned)
        .collect();
    MutationSummary {
        fact_ids,
        entry_ids,
    }
}
