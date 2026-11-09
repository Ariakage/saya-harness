use super::{
    BuildInput, Grouping, ItemRange, MAX_SERIALIZED_BYTES, counting, error, history_digest,
    item_digest,
};
use crate::contract::{BoundaryError, HistoryItem, ToolState};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

/// Host-only proof. Construct only after authenticating durable native-memory
/// tool ownership, or a durable model-request memory exposure/response binding.
/// It intentionally cannot be deserialized from model/MCP response data.
#[derive(Debug, Clone)]
pub enum RetirementOrigin {
    MemoryToolCall {
        call_index: usize,
        call_sha256: String,
        result_index: usize,
        result_sha256: String,
        memory_revision: String,
    },
    InjectedMemoryTurn {
        request_id_sha256: String,
        memory_revision: String,
    },
}

/// A durable, non-memory operation whose mechanical outcome remains useful.
/// There is no arbitrary text/body field. The host verifies its registered
/// identity and ToolRecord state before supplying this binding.
#[derive(Debug, Clone)]
pub struct ObservationBinding {
    pub call_index: usize,
    pub call_sha256: String,
    pub result_index: usize,
    pub result_sha256: String,
    pub registered_tool_name: String,
    pub state: ToolState,
}

#[derive(Debug, Clone)]
pub struct GroupRetirement {
    pub range: ItemRange,
    pub source_sha256: String,
    pub current_memory_revision: String,
    pub origins: Vec<RetirementOrigin>,
    pub observations: Vec<ObservationBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetirementBinding {
    pub range: ItemRange,
    pub source_sha256: String,
    pub projection_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MechanicalObservation {
    pub call_index: usize,
    pub call_sha256: String,
    pub result_index: usize,
    pub result_sha256: String,
    pub tool_name: String,
    pub state: ToolState,
    pub ok: bool,
    pub truncated: bool,
    pub error_present: bool,
    pub exit_code: Option<i32>,
    pub timed_out: Option<bool>,
    pub cancelled: Option<bool>,
}

pub(super) struct PreparedRetirement {
    pub binding: RetirementBinding,
    pub item: HistoryItem,
    pub observations: Vec<MechanicalObservation>,
}

fn digest_valid(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn pair<'a>(
    input: &'a BuildInput<'_>,
    range: ItemRange,
    call_index: usize,
    call_hash: &str,
    result_index: usize,
    result_hash: &str,
) -> Result<
    (
        &'a crate::contract::ToolCall,
        &'a crate::contract::ToolResult,
    ),
    BoundaryError,
> {
    if call_index < range.start
        || call_index >= range.end
        || result_index <= call_index
        || result_index >= range.end
    {
        return Err(error(
            "context_retirement",
            "Retirement operation ordinals are outside their complete source group",
        ));
    }
    let call_item = &input.source_history[call_index];
    let result_item = &input.source_history[result_index];
    let (HistoryItem::ToolCall(call), HistoryItem::ToolResult { call_id, result }) =
        (call_item, result_item)
    else {
        return Err(error(
            "context_retirement",
            "Retirement proof is not a tool call/result pair",
        ));
    };
    if call.call_id != *call_id
        || item_digest(call_item)? != call_hash
        || item_digest(result_item)? != result_hash
    {
        return Err(error(
            "context_retirement",
            "Retirement proof differs from its original operation",
        ));
    }
    Ok((call, result))
}

pub(super) fn prepare(
    input: &BuildInput<'_>,
    grouping: &Grouping,
) -> Result<BTreeMap<usize, PreparedRetirement>, BoundaryError> {
    if input.group_retirements.len() > grouping.groups.len() {
        return Err(error(
            "context_retirement",
            "Too many retirement projections",
        ));
    }
    let mut prepared = BTreeMap::new();
    let groups: BTreeMap<_, _> = grouping
        .groups
        .iter()
        .map(|group| (group.range.start, group))
        .collect();
    let mut projected_bytes = 0u64;
    for retirement in input.group_retirements {
        if !groups
            .get(&retirement.range.start)
            .is_some_and(|group| group.closed && group.range == retirement.range)
            || input
                .protected_ranges
                .iter()
                .any(|range| range.overlaps(retirement.range))
            || retirement.origins.is_empty()
            || retirement.origins.len() > 256
            || retirement.observations.len() > 256
            || !digest_valid(&retirement.current_memory_revision)
            || history_digest(&input.source_history[retirement.range.start..retirement.range.end])?
                != retirement.source_sha256
            || input.source_history[retirement.range.start..retirement.range.end]
                .iter()
                .any(|item| matches!(item, HistoryItem::UserText(_)))
        {
            return Err(error(
                "context_retirement",
                "Retirement is not a source-bound closed unprotected assistant group",
            ));
        }
        let mut memory_results = BTreeSet::new();
        for origin in &retirement.origins {
            match origin {
                RetirementOrigin::MemoryToolCall {
                    call_index,
                    call_sha256,
                    result_index,
                    result_sha256,
                    memory_revision,
                } => {
                    pair(
                        input,
                        retirement.range,
                        *call_index,
                        call_sha256,
                        *result_index,
                        result_sha256,
                    )?;
                    if !digest_valid(memory_revision) || !memory_results.insert(*result_index) {
                        return Err(error(
                            "context_retirement",
                            "Memory operation proof is invalid or repeated",
                        ));
                    }
                }
                RetirementOrigin::InjectedMemoryTurn {
                    request_id_sha256,
                    memory_revision,
                } => {
                    if !digest_valid(request_id_sha256) || !digest_valid(memory_revision) {
                        return Err(error(
                            "context_retirement",
                            "Injected-memory turn proof is invalid",
                        ));
                    }
                }
            }
        }
        let mut observations = Vec::new();
        let mut observed = BTreeSet::new();
        for observation in &retirement.observations {
            let (call, result) = pair(
                input,
                retirement.range,
                observation.call_index,
                &observation.call_sha256,
                observation.result_index,
                &observation.result_sha256,
            )?;
            if memory_results.contains(&observation.result_index)
                || !observed.insert(observation.result_index)
                || observation.registered_tool_name != call.name
                || call.name.len() > 64
                || !call
                    .name
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphabetic)
                || !call
                    .name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
                || !matches!(
                    observation.state,
                    ToolState::Succeeded
                        | ToolState::Failed
                        | ToolState::Denied
                        | ToolState::Unknown
                )
                || (observation.state == ToolState::Succeeded) != result.ok
            {
                return Err(error(
                    "context_retirement",
                    "Retained observation is not a verified non-memory outcome",
                ));
            }
            let data = result.data.as_ref();
            observations.push(MechanicalObservation {
                call_index: observation.call_index,
                call_sha256: observation.call_sha256.clone(),
                result_index: observation.result_index,
                result_sha256: observation.result_sha256.clone(),
                tool_name: observation.registered_tool_name.clone(),
                state: observation.state,
                ok: result.ok,
                truncated: result.truncated,
                error_present: result.error.is_some(),
                exit_code: data
                    .and_then(|value| value.get("exit_code"))
                    .and_then(serde_json::Value::as_i64)
                    .and_then(|value| i32::try_from(value).ok()),
                timed_out: data
                    .and_then(|value| value.get("timed_out"))
                    .and_then(serde_json::Value::as_bool),
                cancelled: data
                    .and_then(|value| value.get("cancelled"))
                    .and_then(serde_json::Value::as_bool),
            });
        }
        observations.sort_by_key(|observation| observation.result_index);
        let envelope = json!({"saya_retired_history_group_v1":{
            "trust":"historical_source_data","reason":"memory_context_changed",
            "source_range":retirement.range,"source_sha256":retirement.source_sha256,
            "current_memory_revision":retirement.current_memory_revision,
            "handling":"Original memory-derived text, arguments, provider state, and result bodies are unavailable in this projection. Retain only the listed mechanical observations. Use current memory and runtime authority.",
            "observations":observations,
        }});
        let measured = counting::measure(&envelope, 256 * 1024)?;
        projected_bytes = projected_bytes
            .checked_add(measured.bytes)
            .filter(|bytes| *bytes <= MAX_SERIALIZED_BYTES)
            .ok_or_else(|| {
                error(
                    "context_limit",
                    "Retirement projections exceed their total byte limit",
                )
            })?;
        let text = serde_json::to_string(&envelope).map_err(|_| {
            error(
                "context_retirement",
                "Retirement envelope cannot be encoded",
            )
        })?;
        let binding = RetirementBinding {
            range: retirement.range,
            source_sha256: retirement.source_sha256.clone(),
            projection_sha256: measured.sha256,
        };
        if prepared
            .insert(
                retirement.range.start,
                PreparedRetirement {
                    binding,
                    item: HistoryItem::UserText(text),
                    observations,
                },
            )
            .is_some()
        {
            return Err(error(
                "context_retirement",
                "Retirement source group is repeated",
            ));
        }
    }
    Ok(prepared)
}
