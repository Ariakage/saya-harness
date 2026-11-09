use super::{ContextReference, error};
use crate::{context_builder::ItemRange, contract::BoundaryError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceLevel {
    UserRequest,
    Observed,
    Claim,
    Inference,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactStatus {
    Active,
    Superseded,
    Uncertain,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fact {
    pub text: String,
    pub source_ids: Vec<String>,
    pub evidence: EvidenceLevel,
    pub status: FactStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MandatoryKind {
    Objective,
    Constraint,
    Remaining,
    Recovery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MandatoryItem {
    pub id: String,
    pub kind: MandatoryKind,
    pub text: String,
    pub source_ids: Vec<String>,
    pub status: FactStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructuredSummary {
    pub schema_version: u8,
    pub covered_range: ItemRange,
    pub objective: Vec<Fact>,
    pub constraints: Vec<Fact>,
    pub decisions: Vec<Fact>,
    pub completed: Vec<Fact>,
    pub remaining: Vec<Fact>,
    pub uncertainties: Vec<Fact>,
    pub required_ids: Vec<String>,
    pub references: Vec<String>,
}
impl StructuredSummary {
    pub(super) fn facts(&self) -> impl Iterator<Item = &Fact> {
        self.objective
            .iter()
            .chain(&self.constraints)
            .chain(&self.decisions)
            .chain(&self.completed)
            .chain(&self.remaining)
            .chain(&self.uncertainties)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SourceRole {
    User,
    Assistant,
    Tool,
    Provider,
    Retired,
    Mechanical,
}

pub(super) struct Validation<'a> {
    pub range: ItemRange,
    pub sources: &'a BTreeMap<String, SourceRole>,
    pub required: Vec<&'a MandatoryItem>,
    pub references: Vec<&'a ContextReference>,
}

pub(super) fn ordinal(source: &str) -> Option<usize> {
    let suffix = source.strip_prefix("history:")?;
    let number = suffix.parse::<usize>().ok()?;
    (number.to_string() == suffix).then_some(number)
}

pub(super) fn source_ids_valid(ids: &[String], sources: &BTreeMap<String, SourceRole>) -> bool {
    !ids.is_empty()
        && ids.len() <= 32
        && ids.iter().all(|id| {
            sources
                .get(id)
                .is_some_and(|role| *role != SourceRole::Retired)
        })
        && ids.iter().collect::<BTreeSet<_>>().len() == ids.len()
}

pub(super) fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:/".contains(&byte))
}

pub(super) fn validate(
    summary: &StructuredSummary,
    context: &Validation<'_>,
) -> Result<(), BoundaryError> {
    if summary.schema_version != 1 || summary.covered_range != context.range {
        return Err(error(
            "compaction_invalid",
            "Summary range or schema version differs from its assigned source",
        ));
    }
    let lists = [
        &summary.objective,
        &summary.constraints,
        &summary.decisions,
        &summary.completed,
        &summary.remaining,
        &summary.uncertainties,
    ];
    if lists.iter().any(|list| list.len() > 64) || summary.facts().count() > 256 {
        return Err(error("compaction_invalid", "Summary has too many facts"));
    }
    let mut cited = BTreeSet::new();
    let mut provenance = BTreeMap::new();
    for fact in summary.facts() {
        if fact.text.trim().is_empty()
            || fact.text.len() > 2048
            || !source_ids_valid(&fact.source_ids, context.sources)
        {
            return Err(error(
                "compaction_invalid",
                "Summary fact text or source references are invalid",
            ));
        }
        for id in &fact.source_ids {
            let index = ordinal(id).ok_or_else(|| {
                error("compaction_invalid", "Summary source identifier is invalid")
            })?;
            if index < context.range.start || index >= context.range.end {
                return Err(error(
                    "compaction_invalid",
                    "Summary cites material outside its assigned source range",
                ));
            }
            let role = context
                .sources
                .get(id)
                .ok_or_else(|| error("compaction_invalid", "Summary source is unavailable"))?;
            if (fact.evidence == EvidenceLevel::UserRequest && *role != SourceRole::User)
                || (fact.evidence == EvidenceLevel::Observed
                    && !matches!(
                        role,
                        SourceRole::Tool | SourceRole::Provider | SourceRole::Mechanical
                    ))
            {
                return Err(error(
                    "compaction_invalid",
                    "Summary evidence level differs from its source role",
                ));
            }
            cited.insert(id);
        }
        let sources: BTreeSet<_> = fact.source_ids.iter().collect();
        if let Some(previous) =
            provenance.insert((&fact.text, sources), (fact.evidence, fact.status))
            && previous != (fact.evidence, fact.status)
        {
            return Err(error(
                "compaction_invalid",
                "Summary assigns conflicting evidence or status to one source fact",
            ));
        }
    }
    for (id, role) in context.sources {
        if *role == SourceRole::User
            && ordinal(id)
                .is_some_and(|index| index >= context.range.start && index < context.range.end)
            && !cited.contains(id)
        {
            return Err(error(
                "compaction_invalid",
                "Summary omitted an original user source",
            ));
        }
    }
    let required: BTreeSet<_> = context.required.iter().map(|item| &item.id).collect();
    let actual: BTreeSet<_> = summary.required_ids.iter().collect();
    if actual != required || actual.len() != summary.required_ids.len() {
        return Err(error(
            "compaction_invalid",
            "Summary mandatory-item identities differ from the host contract",
        ));
    }
    for item in &context.required {
        let facts = match item.kind {
            MandatoryKind::Objective => &summary.objective,
            MandatoryKind::Constraint => &summary.constraints,
            MandatoryKind::Remaining => &summary.remaining,
            MandatoryKind::Recovery => &summary.uncertainties,
        };
        let user_request = item
            .source_ids
            .iter()
            .all(|id| context.sources.get(id) == Some(&SourceRole::User));
        if !facts.iter().any(|fact| {
            fact.text == item.text
                && fact.status == item.status
                && (!user_request || fact.evidence == EvidenceLevel::UserRequest)
                && fact.source_ids.iter().collect::<BTreeSet<_>>()
                    == item.source_ids.iter().collect()
        }) {
            return Err(error(
                "compaction_invalid",
                "Summary changed or omitted a mandatory fact",
            ));
        }
    }
    let expected: BTreeSet<_> = context
        .references
        .iter()
        .map(|reference| &reference.id)
        .collect();
    let actual: BTreeSet<_> = summary.references.iter().collect();
    if actual != expected || actual.len() != summary.references.len() {
        return Err(error(
            "compaction_invalid",
            "Summary changed, omitted, or invented an artifact reference",
        ));
    }
    Ok(())
}

/// Stable JSON schema supplied as data on every summary/merge request. Rust
/// validation additionally checks roles, exact mandatory facts and provenance.
pub fn summary_schema() -> Value {
    let sources = json!({"type":"array","minItems":1,"maxItems":32,"uniqueItems":true,
        "items":{"type":"string","pattern":"^history:[0-9]+$"}});
    let fact = json!({"type":"object","additionalProperties":false,
        "required":["text","source_ids","evidence","status"],"properties":{
            "text":{"type":"string","minLength":1,"maxLength":2048},"source_ids":sources,
            "evidence":{"type":"string","enum":["user_request","observed","claim","inference"]},
            "status":{"type":"string","enum":["active","superseded","uncertain"]}}});
    let facts = json!({"type":"array","maxItems":64,"items":fact});
    json!({"type":"object","additionalProperties":false,
        "required":["schema_version","covered_range","objective","constraints","decisions","completed","remaining","uncertainties","required_ids","references"],
        "properties":{
            "schema_version":{"const":1},
            "covered_range":{"type":"object","additionalProperties":false,"required":["start","end"],
                "properties":{"start":{"type":"integer","minimum":0},"end":{"type":"integer","minimum":1}}},
            "objective":facts,"constraints":facts,"decisions":facts,"completed":facts,"remaining":facts,"uncertainties":facts,
            "required_ids":{"type":"array","uniqueItems":true,"maxItems":64,"items":{"type":"string"}},
            "references":{"type":"array","uniqueItems":true,"maxItems":256,"items":{"type":"string"}}}})
}
