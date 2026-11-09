//! Tool-free, source-bound compaction preparation. Persistence is a host CAS.
//! Current user/mode/permission state is never reconstructed from a summary.

use crate::{
    boundary::{Cancellation, Model, ModelRequest},
    context_builder::{
        self, BuildInput, ContextView, CountingIdentity, ItemRange, MechanicalObservation,
        RequestCounter, RetainedGroup, RetirementBinding, SourceWatermark, TokenCount,
    },
    contract::{BoundaryError, ErrorInfo, HistoryItem, PermissionMode, SessionId, Usage, WorkMode},
    review::SharedBudget,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    io::{self, Write},
    time::Duration,
};
use tokio::{sync::Mutex, time::Instant};

mod model;
mod planning;
mod schema;
#[cfg(test)]
mod tests;

pub use schema::{
    EvidenceLevel, Fact, FactStatus, MandatoryItem, MandatoryKind, StructuredSummary,
    summary_schema,
};

pub const PROMPT_VERSION: &str = "saya-compact-v1";
pub const COMPACT_INSTRUCTIONS: &str = include_str!("../prompts/v1/compact.md");
pub const CONTINUATION_INSTRUCTIONS: &str =
    include_str!("../prompts/v1/compaction-continuation.md");
const MAX_JSON_BYTES: usize = 32 * 1024 * 1024;
const MAX_SUMMARY_BYTES: usize = 64 * 1024;

fn error(code: &str, message: &str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}

#[derive(Debug, Clone)]
pub struct CompactionConfig {
    pub summary_output_tokens: u32,
    pub max_model_calls: usize,
    pub max_chunks: usize,
    pub max_merge_rounds: usize,
    pub max_retries: usize,
    pub connect_timeout: Duration,
    pub idle_timeout: Duration,
    pub request_timeout: Duration,
    pub operation_timeout: Duration,
    pub retry_jitter_max: Duration,
}
impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            summary_output_tokens: 32000,
            max_model_calls: 32,
            max_chunks: 16,
            max_merge_rounds: 4,
            max_retries: 2,
            connect_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(60),
            request_timeout: Duration::from_secs(180),
            operation_timeout: Duration::from_secs(600),
            retry_jitter_max: Duration::from_millis(250),
        }
    }
}
impl CompactionConfig {
    fn validate(&self) -> Result<(), BoundaryError> {
        if self.summary_output_tokens == 0
            || self.max_model_calls == 0
            || self.max_model_calls > 64
            || self.max_chunks == 0
            || self.max_chunks > 32
            || self.max_merge_rounds == 0
            || self.max_merge_rounds > 8
            || self.max_retries > 2
            || [
                self.connect_timeout,
                self.idle_timeout,
                self.request_timeout,
                self.operation_timeout,
            ]
            .iter()
            .any(|duration| duration.is_zero() || *duration > Duration::from_secs(3600))
            || self.retry_jitter_max > Duration::from_secs(1)
        {
            return Err(error("compaction_config", "Compaction limits are invalid"));
        }
        Ok(())
    }
}

/// Metadata authenticated by the host, never inferred from strings in tool data.
/// Keep metadata bounded and include completeness/ownership needed to resume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextReference {
    pub id: String,
    pub source_ids: Vec<String>,
    pub metadata: Value,
}

/// Current source versions let the host invalidate memory/Skill/project views.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextVersion {
    pub namespace: String,
    pub id: String,
    pub revision: u64,
    pub sha256: String,
}

pub struct CompactionInput<'a> {
    pub context: BuildInput<'a>,
    pub session_id: SessionId,
    pub checkpoint_id: &'a str,
    pub prefix_end: usize,
    pub work_mode: WorkMode,
    pub permissions: PermissionMode,
    /// Exact host-selected constraints/remaining tasks from the covered prefix.
    /// They are validated verbatim. They do not grant execution permission.
    pub required_items: &'a [MandatoryItem],
    pub references: &'a [ContextReference],
    pub context_versions: &'a [ContextVersion],
    /// Snapshot at preparation time; the host rechecks current credentials
    /// before committing and emitting any result.
    pub known_secrets: &'a [String],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionStage {
    Summarize,
    Merge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SummaryCallRecord {
    pub request_id: String,
    pub stage: CompactionStage,
    pub attempt: usize,
    pub request_fingerprint: String,
    pub source_range: ItemRange,
    pub duration_ms: u64,
    pub usage: Option<Usage>,
    pub outcome: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactionRecord {
    pub schema_version: u8,
    pub checkpoint_id: String,
    pub session_id: SessionId,
    pub source: SourceWatermark,
    pub covered_range: ItemRange,
    pub source_prefix_sha256: String,
    pub profile: String,
    pub prompt_version: String,
    pub prompt_sha256: String,
    pub summary_sha256: String,
    pub previous_checkpoint: Option<String>,
    pub summarized_groups: Vec<ItemRange>,
    #[serde(default)]
    pub retained_groups: Vec<RetainedGroup>,
    pub context_versions: Vec<ContextVersion>,
    pub calls: Vec<SummaryCallRecord>,
    #[serde(default)]
    pub retirement_manifest: Vec<RetirementBinding>,
    pub target_input_tokens: u64,
    pub projected_input_count: TokenCount,
}
impl CompactionRecord {
    /// Validate a stored record/view pair independently of an ephemeral request.
    pub fn verify_view(&self, view: &ContextView) -> Result<(), BoundaryError> {
        if self.schema_version != 1
            || self.covered_range.start != 0
            || self.covered_range.end > self.source.item_count
            || self.retained_groups.len() > 1
            || self.projected_input_count.tokens > self.target_input_tokens
            || view.covered_prefix != self.covered_range.end
            || view.checkpoint_id != self.checkpoint_id
            || view.profile != self.profile
            || view.identity != self.source.identity
            || view.source_revision != self.source.revision
            || view.source_prefix_sha256 != self.source_prefix_sha256
            || self.prompt_version != PROMPT_VERSION
            || self.prompt_sha256 != digest(COMPACT_INSTRUCTIONS.as_bytes())
            || self.summary_sha256 != digest(&encode(&view.summary, MAX_SUMMARY_BYTES)?)
        {
            return Err(error(
                "compaction_invalid",
                "Prepared checkpoint integrity does not match its record",
            ));
        }
        let committed: CommittedSummary =
            serde_json::from_value(view.summary.clone()).map_err(|_| {
                error(
                    "compaction_invalid",
                    "Stored compaction summary schema is invalid",
                )
            })?;
        if committed.schema_version != 1
            || committed.summary.covered_range != self.covered_range
            || committed.context_versions != self.context_versions
            || committed.retained_groups != self.retained_groups
            || view.retirement_manifest != self.retirement_manifest
            || committed.retirement_manifest != self.retirement_manifest
        {
            return Err(error(
                "compaction_invalid",
                "Stored compaction summary metadata is incompatible",
            ));
        }
        for group in &self.retained_groups {
            if group.range.start >= group.range.end
                || group.range.end - group.range.start != 1
                || group.range.end > self.covered_range.end
                || group.source_sha256.len() != 64
                || !group
                    .source_sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(error(
                    "compaction_invalid",
                    "Retained source-group metadata is invalid",
                ));
            }
        }
        for binding in &self.retirement_manifest {
            if binding.range.start >= binding.range.end
                || binding.range.end > self.covered_range.end
            {
                return Err(error(
                    "compaction_invalid",
                    "Retired source range is invalid",
                ));
            }
        }
        Ok(())
    }
}

/// Shared validation for durable load/apply paths and prepared results.
/// The host separately authenticates the source history and output references.
pub fn validate_committed(
    view: &ContextView,
    record: &CompactionRecord,
) -> Result<(), BoundaryError> {
    record.verify_view(view)
}

/// This result has not been saved and must not be applied until the host commits
/// record and view together with the exact expected source watermark.
#[derive(Debug, Clone)]
pub struct PreparedCompaction {
    pub view: ContextView,
    pub record: CompactionRecord,
    pub projected_request: ModelRequest,
}
impl PreparedCompaction {
    pub fn verify_current(
        &self,
        session_id: SessionId,
        history: &[HistoryItem],
        revision: u64,
        profile: &str,
        identity: &CountingIdentity,
        context_versions: &[ContextVersion],
    ) -> Result<(), BoundaryError> {
        self.verify_integrity()?;
        if session_id != self.record.session_id
            || revision != self.record.source.revision
            || history.len() != self.record.source.item_count
            || profile != self.record.profile
            || *identity != self.record.source.identity
            || context_versions != self.record.context_versions
            || context_builder::history_digest(history)? != self.record.source.history_sha256
        {
            return Err(error(
                "compaction_conflict",
                "Compaction source or current context versions changed",
            ));
        }
        for group in &self.record.retained_groups {
            if context_builder::history_digest(&history[group.range.start..group.range.end])?
                != group.source_sha256
                || !matches!(&history[group.range.start], HistoryItem::UserText(_))
            {
                return Err(error(
                    "compaction_conflict",
                    "Retained original user source has changed",
                ));
            }
        }
        for binding in &self.record.retirement_manifest {
            if context_builder::history_digest(&history[binding.range.start..binding.range.end])?
                != binding.source_sha256
            {
                return Err(error(
                    "compaction_conflict",
                    "Retired source group has changed",
                ));
            }
        }
        Ok(())
    }

    pub fn verify_integrity(&self) -> Result<(), BoundaryError> {
        validate_committed(&self.view, &self.record)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommittedSummary {
    schema_version: u8,
    summary: StructuredSummary,
    required_items: Vec<MandatoryItem>,
    references: Vec<ContextReference>,
    context_versions: Vec<ContextVersion>,
    #[serde(default)]
    retained_groups: Vec<RetainedGroup>,
    #[serde(default)]
    retirement_manifest: Vec<RetirementBinding>,
    #[serde(default)]
    mechanical_observations: Vec<MechanicalObservation>,
}

type CachedResult = (String, Result<PreparedCompaction, BoundaryError>);

/// One per-run coordinator. Concurrent identical preparations share one cached
/// result. A changed source/configuration has a different key. No database
/// transaction is held during model calls. Dropping a task releases the mutex.
pub struct CompactionCoordinator {
    config: CompactionConfig,
    state: Mutex<Option<CachedResult>>,
}
impl CompactionCoordinator {
    pub fn new(config: CompactionConfig) -> Result<Self, BoundaryError> {
        config.validate()?;
        Ok(Self {
            config,
            state: Mutex::new(None),
        })
    }

    pub async fn prepare(
        &self,
        input: CompactionInput<'_>,
        model: &dyn Model,
        budget: &SharedBudget,
        counter: Option<&dyn RequestCounter>,
        cancel: &dyn Cancellation,
    ) -> Result<PreparedCompaction, BoundaryError> {
        if cancel.is_cancelled() {
            return Err(error("cancelled", "Compaction was cancelled"));
        }
        let started = Instant::now();
        let key = planning::cache_key(&input, &self.config)?;
        let mut state = model::bounded(
            async { Ok(self.state.lock().await) },
            cancel,
            budget,
            started + self.config.operation_timeout,
        )
        .await?;
        if let Some((existing, result)) = state.as_ref()
            && *existing == key
        {
            let mut prepared = result.clone()?;
            // A host can supply a different local counter implementation. Never
            // reuse the previous dispatchability judgment without recounting.
            let rebuilt = context_builder::build(
                planning::copy_context(&input.context, Some(&prepared.view)),
                counter,
            )?;
            if !matches!(rebuilt.decision, context_builder::ContextDecision::Ready)
                || rebuilt.accounting.total.tokens > target_tokens(&input)?
            {
                return Err(error(
                    "context_limit",
                    "Cached compaction does not fit the current counting policy",
                ));
            }
            prepared.projected_request = rebuilt.request;
            prepared.record.projected_input_count = rebuilt.accounting.total;
            return Ok(prepared);
        }
        let result = self
            .prepare_inner(&input, model, budget, counter, cancel, started)
            .await;
        *state = Some((key, result.clone()));
        result
    }

    async fn prepare_inner(
        &self,
        input: &CompactionInput<'_>,
        model: &dyn Model,
        budget: &SharedBudget,
        counter: Option<&dyn RequestCounter>,
        cancel: &dyn Cancellation,
        started: Instant,
    ) -> Result<PreparedCompaction, BoundaryError> {
        let plan = planning::plan(input, &self.config, counter)?;
        let deadline = started + self.config.operation_timeout;
        let mut runner = model::Runner {
            model,
            budget,
            counter,
            cancel,
            config: &self.config,
            input,
            deadline,
            calls: Vec::new(),
        };
        let chunks = planning::split(
            &plan.units,
            &plan,
            input,
            &self.config,
            counter,
            CompactionStage::Summarize,
        )?;
        let mut summaries = Vec::new();
        for chunk in chunks {
            summaries.push(
                runner
                    .summarize(chunk, &plan, CompactionStage::Summarize)
                    .await?,
            );
        }
        for _ in 0..self.config.max_merge_rounds {
            if summaries.len() == 1 {
                break;
            }
            let units = planning::summary_units(&summaries)?;
            let batches = planning::split(
                &units,
                &plan,
                input,
                &self.config,
                counter,
                CompactionStage::Merge,
            )?;
            if batches.len() >= summaries.len() {
                return Err(error(
                    "context_limit",
                    "Summary groups cannot fit a reducing merge request",
                ));
            }
            summaries.clear();
            for batch in batches {
                summaries.push(
                    runner
                        .summarize(batch, &plan, CompactionStage::Merge)
                        .await?,
                );
            }
        }
        if summaries.len() != 1 {
            return Err(error(
                "context_limit",
                "Compaction merge-round limit reached",
            ));
        }
        let summary = summaries
            .pop()
            .ok_or_else(|| error("compaction_invalid", "Compaction produced no summary"))?;
        schema::validate(
            &summary,
            &plan.validation_for(ItemRange {
                start: 0,
                end: input.prefix_end,
            }),
        )?;
        let committed = CommittedSummary {
            schema_version: 1,
            summary,
            required_items: plan.required.clone(),
            references: plan.references.clone(),
            context_versions: input.context_versions.to_vec(),
            retained_groups: plan.retained_groups.clone(),
            retirement_manifest: plan.retirement_manifest.clone(),
            mechanical_observations: plan.mechanical_observations.clone(),
        };
        let encoded = encode(&committed, MAX_SUMMARY_BYTES)?;
        ensure_no_secrets(&encoded, input.known_secrets)?;
        let view = ContextView {
            checkpoint_id: input.checkpoint_id.into(),
            source_revision: input.context.revision,
            covered_prefix: input.prefix_end,
            source_prefix_sha256: context_builder::history_digest(
                &input.context.source_history[..input.prefix_end],
            )?,
            profile: input.context.request.profile.clone(),
            identity: input.context.identity.clone(),
            summary: serde_json::from_slice(&encoded)
                .map_err(|_| error("compaction_invalid", "Compaction summary cannot be encoded"))?,
            retirement_manifest: plan.retirement_manifest.clone(),
        };
        let rebuilt =
            context_builder::build(planning::copy_context(&input.context, Some(&view)), counter)?;
        if !matches!(rebuilt.decision, context_builder::ContextDecision::Ready)
            || rebuilt.accounting.total.tokens >= plan.original_input_tokens
        {
            return Err(error(
                "context_limit",
                "Compaction does not produce a smaller dispatchable request",
            ));
        }
        let target = target_tokens(input)?;
        if rebuilt.accounting.total.tokens > target {
            return Err(error(
                "context_limit",
                "Compaction cannot meet its target while retaining required context",
            ));
        }
        let record = CompactionRecord {
            schema_version: 1,
            checkpoint_id: input.checkpoint_id.into(),
            session_id: input.session_id,
            source: plan.watermark,
            covered_range: ItemRange {
                start: 0,
                end: input.prefix_end,
            },
            source_prefix_sha256: view.source_prefix_sha256.clone(),
            profile: view.profile.clone(),
            prompt_version: PROMPT_VERSION.into(),
            prompt_sha256: digest(COMPACT_INSTRUCTIONS.as_bytes()),
            summary_sha256: digest(&encode(&view.summary, MAX_SUMMARY_BYTES)?),
            previous_checkpoint: input
                .context
                .committed_view
                .map(|view| view.checkpoint_id.clone()),
            summarized_groups: plan.group_ranges,
            context_versions: input.context_versions.to_vec(),
            retained_groups: plan.retained_groups,
            calls: runner.calls,
            retirement_manifest: plan.retirement_manifest,
            target_input_tokens: target,
            projected_input_count: rebuilt.accounting.total,
        };
        Ok(PreparedCompaction {
            view,
            record,
            projected_request: rebuilt.request,
        })
    }
}

fn target_tokens(input: &CompactionInput<'_>) -> Result<u64, BoundaryError> {
    let capacity = input.context.budget.input_capacity()?;
    Ok(((u128::from(capacity) * u128::from(input.context.budget.target_per_mille)) / 1000) as u64)
}

fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn ensure_no_secrets(bytes: &[u8], secrets: &[String]) -> Result<(), BoundaryError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| error("compaction_invalid", "Compaction data is not UTF-8"))?;
    if crate::redaction::contains_secret(text, secrets) {
        return Err(error(
            "compaction_sensitive_data",
            "Compaction material contains protected data",
        ));
    }
    Ok(())
}

fn encode<T: Serialize + ?Sized>(value: &T, limit: usize) -> Result<Vec<u8>, BoundaryError> {
    struct Limited {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for Limited {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self
                .bytes
                .len()
                .checked_add(bytes.len())
                .is_none_or(|size| size > self.limit)
            {
                return Err(io::ErrorKind::FileTooLarge.into());
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Limited {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| {
        error(
            "context_limit",
            "Compaction data exceeds its serialized limit",
        )
    })?;
    Ok(writer.bytes)
}
