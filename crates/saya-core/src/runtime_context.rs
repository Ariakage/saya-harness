//! Request projection at the shared runtime entry, before provider dispatch.
use super::{AgentRuntime, error, secrets};
use crate::{
    boundary::ModelRequest,
    context_builder::{
        self, BuildInput, ContextBudget, ContextDecision, ContextView, CountingIdentity, ItemRange,
        ResultProjection,
    },
    contract::{BoundaryError, HistoryItem, Session, ToolResult},
};
use std::{collections::BTreeMap, sync::Mutex};

pub(super) struct ContextState {
    pub budget: ContextBudget,
    pub view: Mutex<Option<ContextView>>,
}
impl ContextState {
    pub fn new(budget: ContextBudget) -> Self {
        Self {
            budget,
            view: Mutex::new(None),
        }
    }
}

impl AgentRuntime {
    pub(super) async fn context_request(
        &self,
        session: &Session,
        request: ModelRequest,
    ) -> Result<ModelRequest, BoundaryError> {
        let Some(state) = &self.context_state else {
            return Ok(request);
        };
        let identity = CountingIdentity {
            protocol: session.protocol,
            model: session.model.clone(),
            adapter_version: crate::provider::COUNTING_ADAPTER_VERSION.into(),
        };
        let records = secrets::redact(
            self.store.output_references(session.id).await?,
            &self.secret_source,
        )?;
        let mut by_call: BTreeMap<String, Vec<_>> = BTreeMap::new();
        for record in records {
            if record.scope.session_id != session.id {
                return Err(error(
                    "output_registration",
                    "Host output registry has a different session",
                ));
            }
            by_call
                .entry(record.scope.call_id.clone())
                .or_default()
                .push(record);
        }
        let mut projections = Vec::new();
        for (source_index, item) in session.history.iter().enumerate() {
            if let HistoryItem::ToolResult { call_id, result } = item
                && let Some(outputs) = by_call.get(call_id)
                && serde_json::to_vec(result)
                    .map_err(|_| error("context_projection", "Tool result cannot be measured"))?
                    .len()
                    > 16 * 1024
            {
                let replacement = ToolResult {
                    ok: result.ok,
                    error: result.error.clone(),
                    truncated: result.truncated
                        || outputs.iter().any(|output| !output.artifact_complete),
                    data: Some(
                        serde_json::json!({"output_refs":outputs,"projection":"Retrieve required retained evidence using output_search and output_read. Inspect completeness and gaps."}),
                    ),
                };
                projections.push(ResultProjection {
                    source_index,
                    source_item_sha256: context_builder::item_digest(item)?,
                    call_id: call_id.clone(),
                    replacement,
                    outputs: outputs.clone(),
                });
            }
        }
        // Active job origins and observations stay verbatim. A full live range
        // is conservative until a closed safe cut can retain their exact group.
        let protected = if self.jobs.running_count()? == 0 || session.history.is_empty() {
            Vec::new()
        } else {
            vec![ItemRange {
                start: 0,
                end: session.history.len(),
            }]
        };
        let view = state
            .view
            .lock()
            .map_err(|_| error("context_view", "Context view lock failed"))?
            .clone();
        let built = context_builder::build(
            BuildInput {
                history_offset: request.history.len().saturating_sub(session.history.len()),
                request: &request,
                identity: &identity,
                budget: state.budget,
                source_history: &session.history,
                revision: session.history.len() as u64,
                committed_view: view.as_ref(),
                protected_ranges: &protected,
                output_projections: &projections,
                group_retirements: &[],
            },
            self.model.request_counter(),
        )?;
        match built.decision {
            ContextDecision::Ready
            | ContextDecision::NeedsCompact {
                exceeds_input_capacity: false,
                ..
            } => Ok(built.request),
            ContextDecision::NeedsCompact { .. } | ContextDecision::ContextLimit { .. } => {
                Err(error(
                    "context_limit",
                    "Required context exceeds the configured input capacity",
                ))
            }
            ContextDecision::AwaitingToolResults { .. } => Err(error(
                "invalid_history",
                "Context contains an unfinished tool group",
            )),
        }
    }
}
