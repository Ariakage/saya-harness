use super::*;
use crate::global_memory::{MemoryEntry, MemoryFact, MemorySourceRef};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryFactReview {
    pub fact_id: String,
    pub change: String,
    pub scope: MemoryScope,
    pub before_text: Option<String>,
    pub after_text: Option<String>,
    pub before_digest: Option<String>,
    pub after_digest: Option<String>,
    pub kind: MemoryKind,
    pub aliases: Vec<String>,
    pub derived_from: Vec<String>,
    pub sources: Vec<MemorySourceRef>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntryReview {
    pub entry_id: String,
    pub scope: MemoryScope,
    pub before_title: Option<String>,
    pub after_title: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryReviewMetadata {
    pub schema_version: u32,
    pub tool: String,
    pub operation_id: Option<String>,
    pub expected_revision: Option<String>,
    pub scope: Option<MemoryScope>,
    pub arguments_digest: String,
    pub host_request_digest: Option<String>,
    pub latest_user: Option<MemoryEvidence>,
    pub evidence: Vec<MemoryEvidence>,
    pub changes: Vec<MemoryFactReview>,
    pub entries: Vec<MemoryEntryReview>,
    pub tool_evidence_kept_in_workspace: bool,
    pub already_committed: Option<MemoryOperationReceipt>,
    pub truncated: bool,
}

fn fact_digest(fact: &MemoryFact, max: usize) -> Result<String, BoundaryError> {
    evidence::hash(fact, max, &AtomicBool::new(false))
}
fn facts(entries: &[MemoryEntry]) -> BTreeMap<&str, (&MemoryEntry, &MemoryFact)> {
    entries
        .iter()
        .flat_map(|entry| {
            entry
                .facts
                .iter()
                .map(move |fact| (fact.id.as_str(), (entry, fact)))
        })
        .collect()
}
fn diff(
    proposal: &MemoryProposal,
    limits: ManagedMemoryLimits,
) -> Result<(Vec<MemoryFactReview>, Vec<MemoryEntryReview>), BoundaryError> {
    let before = facts(&proposal.before_entries);
    let after = facts(&proposal.after_entries);
    let ids = before
        .keys()
        .chain(after.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    let mut changes = Vec::new();
    for id in ids {
        let old = before.get(id).copied();
        let new = after.get(id).copied();
        if old.map(|(_, fact)| fact) == new.map(|(_, fact)| fact) {
            continue;
        }
        let (entry, fact) = new
            .or(old)
            .ok_or_else(|| failure("memory_review_invalid", "Memory diff has no source record"))?;
        changes.push(MemoryFactReview {
            fact_id: id.into(),
            change: match (old, new) {
                (None, _) => "added",
                (_, None) => "removed",
                _ => "changed",
            }
            .into(),
            scope: entry.scope.clone(),
            before_text: old.map(|(_, fact)| fact.text.clone()),
            after_text: new.map(|(_, fact)| fact.text.clone()),
            before_digest: old
                .map(|(_, fact)| fact_digest(fact, limits.max_history_bytes))
                .transpose()?,
            after_digest: new
                .map(|(_, fact)| fact_digest(fact, limits.max_history_bytes))
                .transpose()?,
            kind: fact.kind,
            aliases: fact.aliases.clone(),
            derived_from: fact.derived_from.clone(),
            sources: fact.sources.clone(),
        });
    }
    let mut entries = Vec::new();
    let entry_ids = proposal
        .before_entries
        .iter()
        .chain(&proposal.after_entries)
        .map(|entry| entry.id.as_str())
        .collect::<BTreeSet<_>>();
    for id in entry_ids {
        let old = proposal.before_entries.iter().find(|entry| entry.id == id);
        let new = proposal.after_entries.iter().find(|entry| entry.id == id);
        if old.map(|entry| &entry.title) == new.map(|entry| &entry.title) {
            continue;
        }
        let entry = new.or(old).ok_or_else(|| {
            failure(
                "memory_review_invalid",
                "Memory entry diff has no source record",
            )
        })?;
        entries.push(MemoryEntryReview {
            entry_id: id.into(),
            scope: entry.scope.clone(),
            before_title: old.map(|entry| entry.title.clone()),
            after_title: new.map(|entry| entry.title.clone()),
        });
    }
    Ok((changes, entries))
}
fn clipped(text: &mut String, max: usize) -> bool {
    let mut end = max.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let truncated = end < text.len();
    text.truncate(end);
    truncated
}
fn clip_value(value: &mut Value, max: usize) -> bool {
    match value {
        Value::String(text) => clipped(text, max),
        Value::Object(fields) => {
            let mut changed = false;
            for value in fields.values_mut() {
                changed |= clip_value(value, max);
            }
            changed
        }
        _ => false,
    }
}
fn clip_review(review: &mut MemoryReviewMetadata, max: usize) {
    for evidence in review
        .evidence
        .iter_mut()
        .chain(review.latest_user.iter_mut())
    {
        evidence.excerpt_truncated |= clip_value(&mut evidence.excerpt, max);
        review.truncated |= evidence.excerpt_truncated || evidence.result_truncated;
    }
    for change in &mut review.changes {
        for text in change
            .before_text
            .iter_mut()
            .chain(change.after_text.iter_mut())
            .chain(change.aliases.iter_mut())
        {
            review.truncated |= clipped(text, max);
        }
    }
    for entry in &mut review.entries {
        for title in entry
            .before_title
            .iter_mut()
            .chain(entry.after_title.iter_mut())
        {
            review.truncated |= clipped(title, max);
        }
    }
}

pub(super) struct ReviewInput<'a> {
    pub(super) parsed: &'a args::Parsed,
    pub(super) arguments_digest: &'a str,
    pub(super) host_request_digest: Option<&'a str>,
    pub(super) scope: Option<MemoryScope>,
    pub(super) bundle: Option<&'a evidence::Bundle>,
    pub(super) proposal: Option<&'a MemoryProposal>,
    pub(super) receipt: Option<&'a MemoryOperationReceipt>,
    pub(super) tool_evidence_kept_in_workspace: bool,
}
pub(super) fn build(
    tools: &ManagedMemoryTools,
    call: &ToolCall,
    input: ReviewInput<'_>,
) -> Result<(Value, bool, evidence::RedactionProbe), BoundaryError> {
    let (changes, entries) = input
        .proposal
        .map(|proposal| diff(proposal, tools.limits))
        .transpose()?
        .unwrap_or_default();
    let base = MemoryReviewMetadata {
        schema_version: 1,
        tool: call.name.clone(),
        operation_id: input.parsed.operation_id().map(str::to_owned),
        expected_revision: input.parsed.expected_revision().map(str::to_owned),
        scope: input.scope,
        arguments_digest: input.arguments_digest.into(),
        host_request_digest: input.host_request_digest.map(str::to_owned),
        latest_user: input.bundle.and_then(|bundle| bundle.latest_user.clone()),
        evidence: input
            .bundle
            .map(|bundle| bundle.selected.values().cloned().collect())
            .unwrap_or_default(),
        changes,
        entries,
        tool_evidence_kept_in_workspace: input.tool_evidence_kept_in_workspace,
        already_committed: input.receipt.cloned(),
        truncated: false,
    };
    let raw = serde_json::to_value(base).map_err(|_| {
        failure(
            "memory_review_invalid",
            "Memory review metadata cannot be encoded",
        )
    })?;
    let (safe, probe) = evidence::probe(tools, raw)?;
    let base: MemoryReviewMetadata = serde_json::from_value(safe).map_err(|_| {
        failure(
            "memory_review_invalid",
            "Memory review metadata cannot be safely projected",
        )
    })?;
    let mut budget = tools.limits.max_evidence_excerpt_bytes;
    loop {
        let mut candidate = base.clone();
        clip_review(&mut candidate, budget);
        let value = serde_json::to_value(&candidate).map_err(|_| {
            failure(
                "memory_review_invalid",
                "Memory review metadata cannot be encoded",
            )
        })?;
        let value = tools.store.redact_review_value(value)?;
        if encode(&value, tools.limits.max_review_bytes).is_ok() {
            return Ok((value, candidate.truncated, probe));
        }
        if budget == 0 {
            return Err(failure(
                "memory_review_limit",
                "Memory review identity metadata exceeds its byte budget",
            ));
        }
        budget /= 2;
    }
}
