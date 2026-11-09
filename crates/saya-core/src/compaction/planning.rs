use super::{
    COMPACT_INSTRUCTIONS, CommittedSummary, CompactionConfig, CompactionInput, CompactionStage,
    ContextReference, MAX_JSON_BYTES, MAX_SUMMARY_BYTES, MandatoryItem, StructuredSummary, digest,
    encode, ensure_no_secrets, error,
    schema::{self, SourceRole, Validation},
};
use crate::{
    boundary::ModelRequest,
    context_builder::{
        self, BuildInput, ContextBudget, ContextDecision, ContextView, ItemRange,
        MechanicalObservation, RequestCounter, RetainedGroup, RetentionReason, RetirementBinding,
        SourceProjectionKind, SourceWatermark,
    },
    contract::{BoundaryError, HistoryItem, ModelPurpose},
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub(super) struct Unit {
    pub range: ItemRange,
    pub content: Vec<Value>,
}

pub(super) struct Plan {
    pub units: Vec<Unit>,
    pub sources: BTreeMap<String, SourceRole>,
    pub required: Vec<MandatoryItem>,
    pub references: Vec<ContextReference>,
    pub watermark: SourceWatermark,
    pub group_ranges: Vec<ItemRange>,
    pub original_input_tokens: u64,
    pub retained_groups: Vec<RetainedGroup>,
    pub retirement_manifest: Vec<RetirementBinding>,
    pub mechanical_observations: Vec<MechanicalObservation>,
}
impl Plan {
    pub fn validation_for(&self, range: ItemRange) -> Validation<'_> {
        let within = |ids: &[String]| {
            ids.iter().all(|id| {
                schema::ordinal(id).is_some_and(|index| index >= range.start && index < range.end)
            })
        };
        Validation {
            range,
            sources: &self.sources,
            required: self
                .required
                .iter()
                .filter(|item| within(&item.source_ids))
                .collect(),
            references: self
                .references
                .iter()
                .filter(|item| within(&item.source_ids))
                .collect(),
        }
    }
}

pub(super) fn copy_context<'a>(
    context: &'a BuildInput<'_>,
    view: Option<&'a ContextView>,
) -> BuildInput<'a> {
    BuildInput {
        request: context.request,
        identity: context.identity,
        budget: context.budget,
        source_history: context.source_history,
        history_offset: context.history_offset,
        revision: context.revision,
        committed_view: view,
        protected_ranges: context.protected_ranges,
        output_projections: context.output_projections,
        group_retirements: context.group_retirements,
    }
}

fn metadata_depth(value: &Value) -> Result<(), BoundaryError> {
    let mut stack = vec![(value, 0usize)];
    let mut visited = 0usize;
    while let Some((value, depth)) = stack.pop() {
        visited += 1;
        if depth > 64 || visited + stack.len() > 65_536 {
            return Err(error(
                "context_limit",
                "Compaction metadata exceeds its structural limit",
            ));
        }
        match value {
            Value::Array(values) => {
                if values.len() + visited + stack.len() > 65_536 {
                    return Err(error(
                        "context_limit",
                        "Compaction metadata has too many values",
                    ));
                }
                stack.extend(values.iter().map(|value| (value, depth + 1)));
            }
            Value::Object(values) => {
                if values.len() + visited + stack.len() > 65_536 {
                    return Err(error(
                        "context_limit",
                        "Compaction metadata has too many values",
                    ));
                }
                stack.extend(values.values().map(|value| (value, depth + 1)));
            }
            _ => {}
        }
    }
    Ok(())
}

pub(super) fn cache_key(
    input: &CompactionInput<'_>,
    config: &CompactionConfig,
) -> Result<String, BoundaryError> {
    if !schema::identifier(input.checkpoint_id) {
        return Err(error(
            "compaction_invalid",
            "Invalid compaction checkpoint identity",
        ));
    }
    if input.required_items.len() > 64
        || input.references.len() > 256
        || input.context_versions.len() > 256
    {
        return Err(error(
            "context_limit",
            "Compaction host metadata exceeds its limit",
        ));
    }
    let mut versions = BTreeSet::new();
    for version in input.context_versions {
        if !schema::identifier(&version.namespace)
            || version.namespace.len() > 64
            || version.id.is_empty()
            || version.id.len() > 512
            || version.id.chars().any(char::is_control)
            || !versions.insert((&version.namespace, &version.id))
            || version.sha256.len() != 64
            || !version
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(error(
                "compaction_invalid",
                "Context source versions are invalid or duplicated",
            ));
        }
    }
    for reference in input.references {
        metadata_depth(&reference.metadata)?;
    }
    let projected = context_builder::build(
        copy_context(&input.context, input.context.committed_view),
        None,
    )?;
    let request = context_builder::request_fingerprint(&projected.request, input.context.identity)?;
    let history = context_builder::history_digest(input.context.source_history)?;
    let metadata = encode(
        &(
            input.required_items,
            input.references,
            input.context_versions,
        ),
        MAX_SUMMARY_BYTES,
    )?;
    ensure_no_secrets(&metadata, input.known_secrets)?;
    let secrets_hash = digest(&encode(input.known_secrets, 1024 * 1024)?);
    let config_identity = format!("{config:?}");
    // A proposed checkpoint ID does not fork an otherwise identical operation.
    // The first successful preparation supplies the ID shared by all waiters.
    Ok(digest(&encode(
        &(
            input.session_id,
            input.context.revision,
            input.context.budget,
            input.prefix_end,
            input.context.history_offset,
            request,
            history,
            input.work_mode,
            input.permissions,
            metadata,
            input.context.protected_ranges,
            input.context.committed_view,
            config_identity,
            secrets_hash,
        ),
        MAX_JSON_BYTES,
    )?))
}

pub(super) fn plan(
    input: &CompactionInput<'_>,
    config: &CompactionConfig,
    counter: Option<&dyn RequestCounter>,
) -> Result<Plan, BoundaryError> {
    let built = context_builder::build(
        copy_context(&input.context, input.context.committed_view),
        counter,
    )?;
    if matches!(built.decision, ContextDecision::ContextLimit { .. }) {
        return Err(error(
            "context_limit",
            "Required request content cannot fit even after prefix compaction",
        ));
    }
    if !built.grouping.open_call_ids.is_empty()
        || input.prefix_end == 0
        || !built
            .grouping
            .is_safe_prefix(input.prefix_end, input.context.protected_ranges)
    {
        return Err(error(
            "compaction_unsafe",
            "Compaction prefix is not a closed unprotected protocol boundary",
        ));
    }
    let covered = input
        .context
        .committed_view
        .map_or(0, |view| view.covered_prefix);
    if input.prefix_end <= covered {
        return Err(error(
            "compaction_conflict",
            "Compaction prefix does not advance the current checkpoint",
        ));
    }
    let mut sources = BTreeMap::new();
    for (index, item) in input.context.source_history[..input.prefix_end]
        .iter()
        .enumerate()
    {
        let role = match item {
            HistoryItem::UserText(_) => SourceRole::User,
            HistoryItem::ToolResult { .. } => SourceRole::Tool,
            HistoryItem::ProviderItem { .. } => SourceRole::Provider,
            _ => SourceRole::Assistant,
        };
        sources.insert(format!("history:{index}"), role);
    }
    for retirement in input
        .context
        .group_retirements
        .iter()
        .filter(|item| item.range.end <= input.prefix_end)
    {
        for index in retirement.range.start..retirement.range.end {
            sources.insert(format!("history:{index}"), SourceRole::Retired);
        }
        for observation in &retirement.observations {
            sources.insert(
                format!("history:{}", observation.result_index),
                SourceRole::Mechanical,
            );
        }
    }
    let mut required = BTreeMap::new();
    let mut references = BTreeMap::new();
    let mut units = Vec::new();
    if let Some(view) = input.context.committed_view {
        let previous: CommittedSummary =
            serde_json::from_value(view.summary.clone()).map_err(|_| {
                error(
                    "compaction_invalid",
                    "Previous context summary schema is invalid",
                )
            })?;
        if previous.schema_version != 1 || previous.context_versions != input.context_versions {
            return Err(error(
                "compaction_conflict",
                "Previous summary context versions are stale",
            ));
        }
        validate_metadata(&previous.required_items, &previous.references, &sources)?;
        if previous
            .references
            .iter()
            .flat_map(|reference| &reference.source_ids)
            .any(|id| schema::ordinal(id).is_none_or(|index| index >= covered))
        {
            return Err(error(
                "compaction_invalid",
                "Previous summary reference is outside its covered source range",
            ));
        }
        let prior = Validation {
            range: ItemRange {
                start: 0,
                end: covered,
            },
            sources: &sources,
            required: previous.required_items.iter().collect(),
            references: previous.references.iter().collect(),
        };
        schema::validate(&previous.summary, &prior)?;
        for item in &previous.required_items {
            required.insert(item.id.clone(), item.clone());
        }
        for item in &previous.references {
            references.insert(item.id.clone(), item.clone());
        }
        let mut content =
            vec![json!({"prior_summary_data":previous,"checkpoint_id":view.checkpoint_id})];
        for mapping in built
            .source_projections
            .iter()
            .filter(|item| item.kind == SourceProjectionKind::RetainedOriginal)
        {
            for (offset, index) in
                (mapping.source_range.start..mapping.source_range.end).enumerate()
            {
                content.push(document(
                    index,
                    &input.context.source_history[index],
                    &built.request.history[mapping.projected_range.start + offset],
                )?);
            }
        }
        units.push(Unit {
            range: ItemRange {
                start: 0,
                end: covered,
            },
            content,
        });
    }
    validate_metadata(input.required_items, input.references, &sources)?;
    for item in input.required_items {
        if let Some(previous) = required.get(&item.id)
            && (previous.text != item.text
                || previous.source_ids != item.source_ids
                || previous.kind != item.kind)
        {
            return Err(error(
                "compaction_conflict",
                "Mandatory item identity was reused for different source content",
            ));
        }
        required.insert(item.id.clone(), item.clone());
    }
    for item in input.references {
        if let Some(previous) = references.get(&item.id)
            && previous != item
        {
            return Err(error(
                "compaction_conflict",
                "Artifact reference identity was reused with different metadata",
            ));
        }
        references.insert(item.id.clone(), item.clone());
    }
    let required: Vec<_> = required.into_values().collect();
    let references: Vec<_> = references.into_values().collect();
    validate_metadata(&required, &references, &sources)?;
    let mut group_ranges = Vec::new();
    for group in &built.grouping.groups {
        if group.range.start < covered || group.range.end > input.prefix_end {
            continue;
        }
        let mut content = Vec::new();
        for mapping in built.source_projections.iter().filter(|item| {
            item.source_range.start >= group.range.start
                && item.source_range.end <= group.range.end
                && matches!(
                    item.kind,
                    SourceProjectionKind::Original
                        | SourceProjectionKind::OutputReference
                        | SourceProjectionKind::RetiredGroup
                )
        }) {
            if mapping.kind == SourceProjectionKind::RetiredGroup {
                let HistoryItem::UserText(text) =
                    &built.request.history[mapping.projected_range.start]
                else {
                    return Err(error(
                        "compaction_invalid",
                        "Retired group projection has an invalid carrier",
                    ));
                };
                let envelope: Value = serde_json::from_str(text).map_err(|_| {
                    error(
                        "compaction_invalid",
                        "Retired group projection cannot be encoded",
                    )
                })?;
                content.push(json!({"role":"retired_group","source_range":mapping.source_range,"display":envelope}));
                for observation in &mapping.observations {
                    content.push(json!({"source_id":format!("history:{}",observation.result_index),
                        "source_sha256":observation.result_sha256,"role":"tool_observation","display":observation}));
                }
            } else {
                for (offset, index) in
                    (mapping.source_range.start..mapping.source_range.end).enumerate()
                {
                    content.push(document(
                        index,
                        &input.context.source_history[index],
                        &built.request.history[mapping.projected_range.start + offset],
                    )?);
                }
            }
        }
        group_ranges.push(group.range);
        units.push(Unit {
            range: group.range,
            content,
        });
    }
    // A required fact/reference spanning several groups is kept in one chunk.
    let spans: Vec<_> = required
        .iter()
        .map(|item| &item.source_ids)
        .chain(references.iter().map(|item| &item.source_ids))
        .filter_map(|ids| {
            let indices: Vec<_> = ids.iter().filter_map(|id| schema::ordinal(id)).collect();
            Some(ItemRange {
                start: *indices.iter().min()?,
                end: indices.iter().max()? + 1,
            })
        })
        .collect();
    let units = coalesce(units, &spans);
    let mut retained_groups = Vec::new();
    if let Some(index) = built
        .grouping
        .latest_user_index
        .filter(|index| *index < input.prefix_end)
    {
        let range = ItemRange {
            start: index,
            end: index + 1,
        };
        retained_groups.push(RetainedGroup {
            range,
            source_sha256: context_builder::history_digest(
                &input.context.source_history[range.start..range.end],
            )?,
            reason: RetentionReason::LatestUser,
        });
    }
    let _ = summary_budget(input, config).input_capacity()?;
    let retirement_manifest = built
        .retirement_manifest
        .iter()
        .filter(|binding| binding.range.end <= input.prefix_end)
        .cloned()
        .collect();
    let mechanical_observations = built
        .mechanical_observations
        .iter()
        .filter(|item| item.result_index < input.prefix_end)
        .cloned()
        .collect();
    Ok(Plan {
        units,
        sources,
        required,
        references,
        watermark: built.watermark,
        group_ranges,
        original_input_tokens: built.accounting.total.tokens,
        retained_groups,
        retirement_manifest,
        mechanical_observations,
    })
}

fn validate_metadata(
    required: &[MandatoryItem],
    references: &[ContextReference],
    sources: &BTreeMap<String, SourceRole>,
) -> Result<(), BoundaryError> {
    if required.len() > 64 || references.len() > 256 {
        return Err(error(
            "context_limit",
            "Compaction preservation metadata exceeds its item limit",
        ));
    }
    let mut identities = BTreeSet::new();
    for item in required {
        if !schema::identifier(&item.id)
            || !identities.insert(&item.id)
            || item.text.trim().is_empty()
            || item.text.len() > 2048
            || !schema::source_ids_valid(&item.source_ids, sources)
            || item
                .source_ids
                .iter()
                .any(|id| sources.get(id) == Some(&SourceRole::Mechanical))
        {
            return Err(error(
                "compaction_invalid",
                "Mandatory item has invalid text, identity, or source references",
            ));
        }
    }
    let mut identities = BTreeSet::new();
    for reference in references {
        if !schema::identifier(&reference.id)
            || !identities.insert(&reference.id)
            || !schema::source_ids_valid(&reference.source_ids, sources)
            || reference
                .source_ids
                .iter()
                .any(|id| sources.get(id) == Some(&SourceRole::Mechanical))
        {
            return Err(error(
                "compaction_invalid",
                "Artifact reference has invalid identity or sources",
            ));
        }
        metadata_depth(&reference.metadata)?;
    }
    encode(&(required, references), MAX_SUMMARY_BYTES)?;
    Ok(())
}

fn document(
    index: usize,
    original: &HistoryItem,
    projected: &HistoryItem,
) -> Result<Value, BoundaryError> {
    let (role, display) = match projected {
        HistoryItem::UserText(text) => ("user", json!({"text":text})),
        HistoryItem::AssistantText(text) => ("assistant", json!({"text":text})),
        HistoryItem::AssistantPartial(text) => {
            ("assistant_partial", json!({"text":text,"complete":false}))
        }
        HistoryItem::ToolCall(call) => ("assistant_tool_call", json!({"call":call})),
        HistoryItem::ToolResult { call_id, result } => (
            "tool_observation",
            json!({"call_id":call_id,"result":result}),
        ),
        HistoryItem::ProviderItem { protocol, value } => (
            "provider_state",
            json!({
            "protocol":protocol,"provider_type":value.get("type").and_then(Value::as_str),
            "opaque_state_preserved_by_host":true}),
        ),
    };
    Ok(
        json!({"source_id":format!("history:{index}"),"source_sha256":context_builder::item_digest(original)?,
        "role":role,"display":display}),
    )
}

fn coalesce(units: Vec<Unit>, spans: &[ItemRange]) -> Vec<Unit> {
    let mut result: Vec<Unit> = Vec::new();
    for unit in units {
        let join = result.last().is_some_and(|previous| {
            spans
                .iter()
                .any(|span| span.start < unit.range.start && span.end > previous.range.end)
        });
        if join && let Some(previous) = result.last_mut() {
            previous.range.end = unit.range.end;
            previous.content.extend(unit.content);
        } else {
            result.push(unit);
        }
    }
    result
}

pub(super) fn summary_budget(
    input: &CompactionInput<'_>,
    config: &CompactionConfig,
) -> ContextBudget {
    ContextBudget::new(
        input.context.budget.window_tokens,
        u64::from(config.summary_output_tokens),
        input.context.budget.safety_margin,
    )
}

pub(super) fn request_for(
    units: &[Unit],
    plan: &Plan,
    input: &CompactionInput<'_>,
    config: &CompactionConfig,
    stage: CompactionStage,
) -> Result<ModelRequest, BoundaryError> {
    let range = ItemRange {
        start: units
            .first()
            .ok_or_else(|| error("compaction_invalid", "Compaction chunk is empty"))?
            .range
            .start,
        end: units
            .last()
            .ok_or_else(|| error("compaction_invalid", "Compaction chunk is empty"))?
            .range
            .end,
    };
    let validation = plan.validation_for(range);
    let mut data = json!({"stage":stage,"assigned_source_range":range,
        "effective_state":{"work_mode":input.work_mode,"permissions":input.permissions},
        "required_items":validation.required,"references":validation.references,
        "source_units":units.iter().map(|unit| &unit.content).collect::<Vec<_>>(),
        "output_schema":schema::summary_schema(),"text_max_utf8_bytes":2048,
        "rules":["Cite at least one fact for every original user source in the assigned range.",
            "Echo every supplied required item verbatim in its matching fact category, preserving status and source IDs, and list its ID in required_ids.",
            "List every supplied reference ID exactly once. Copy no opaque provider state into facts.",
            "User-request facts cite only original user sources. Observed facts cite tool or provider observations.",
            "During merge preserve original source IDs, evidence levels, uncertainties, and applicable exact required facts."]});
    crate::redaction::redact(&mut data, input.known_secrets).map_err(|_| {
        error(
            "compaction_sensitive_data",
            "Compaction sources cannot be safely redacted",
        )
    })?;
    let encoded = encode(&data, MAX_JSON_BYTES)?;
    ensure_no_secrets(&encoded, input.known_secrets)?;
    Ok(ModelRequest {
        request_id: uuid::Uuid::now_v7().to_string(),
        purpose: ModelPurpose::Compaction,
        profile: input.context.request.profile.clone(),
        instructions: COMPACT_INSTRUCTIONS.into(),
        dynamic_instructions: None,
        max_output_tokens: Some(config.summary_output_tokens),
        history: vec![HistoryItem::UserText(String::from_utf8(encoded).map_err(
            |_| error("compaction_invalid", "Compaction request is not UTF-8"),
        )?)],
        tools: Vec::new(),
    })
}

pub(super) fn fits(
    request: &ModelRequest,
    input: &CompactionInput<'_>,
    config: &CompactionConfig,
    counter: Option<&dyn RequestCounter>,
) -> Result<bool, BoundaryError> {
    Ok(fitted_request(request, input, config, counter)?.is_some())
}

pub(super) fn fitted_request(
    request: &ModelRequest,
    input: &CompactionInput<'_>,
    config: &CompactionConfig,
    counter: Option<&dyn RequestCounter>,
) -> Result<Option<ModelRequest>, BoundaryError> {
    let built = context_builder::build(
        BuildInput {
            request,
            identity: input.context.identity,
            budget: summary_budget(input, config),
            source_history: &request.history,
            history_offset: 0,
            revision: input.context.revision,
            committed_view: None,
            protected_ranges: &[],
            output_projections: &[],
            group_retirements: &[],
        },
        counter,
    )?;
    Ok(matches!(built.decision, ContextDecision::Ready).then_some(built.request))
}

pub(super) fn split(
    units: &[Unit],
    plan: &Plan,
    input: &CompactionInput<'_>,
    config: &CompactionConfig,
    counter: Option<&dyn RequestCounter>,
    stage: CompactionStage,
) -> Result<Vec<Vec<Unit>>, BoundaryError> {
    let mut batches = Vec::new();
    let mut start = 0usize;
    while start < units.len() {
        if batches.len() >= config.max_chunks {
            return Err(error("context_limit", "Compaction chunk limit reached"));
        }
        let fits_range = |end: usize| -> Result<bool, BoundaryError> {
            let candidate = request_for(&units[start..end], plan, input, config, stage.clone())
                .and_then(|request| fits(&request, input, config, counter));
            match candidate {
                Err(failure) if failure.0.code == "context_limit" => Ok(false),
                result => result,
            }
        };
        let mut low = start + 1;
        if !fits_range(low)? {
            return Err(error(
                "context_limit",
                "One complete protocol group cannot fit a summary request",
            ));
        }
        let mut high = units.len();
        // Bound local counting work instead of serializing every growing prefix.
        // Every selected batch is verified to fit, even with a heuristic counter.
        while low < high {
            let middle = low + (high - low).div_ceil(2);
            if fits_range(middle)? {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        batches.push(units[start..low].to_vec());
        start = low;
    }
    if batches.is_empty() {
        return Err(error("context_limit", "Compaction chunk limit reached"));
    }
    Ok(batches)
}

pub(super) fn summary_units(summaries: &[StructuredSummary]) -> Result<Vec<Unit>, BoundaryError> {
    summaries
        .iter()
        .map(|summary| {
            let content = serde_json::to_value(summary).map_err(|_| {
                error(
                    "compaction_invalid",
                    "Summary merge input cannot be encoded",
                )
            })?;
            Ok(Unit {
                range: summary.covered_range,
                content: vec![json!({"prior_summary_data":content})],
            })
        })
        .collect()
}
