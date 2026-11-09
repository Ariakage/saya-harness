//! Per-run model and tool observations. Missing provider usage stays unknown.
use crate::{
    boundary::{
        Cancellation, Model, ModelRequest, ModelStream, ModelUpdate, Operation, StoreChange,
    },
    config::Pricing,
    contract::{Artifact, BoundaryError, ModelPurpose, Session, ToolResult, Usage},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};
use tracing::Instrument;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunMetrics {
    pub execution_requests: u64,
    pub review_requests: u64,
    #[serde(default)]
    pub compaction_requests: u64,
    pub tool_calls: u64,
    pub tool_executions: u64,
    pub duration_ms: u64,
    pub execution_usage: Option<Usage>,
    pub review_usage: Option<Usage>,
    #[serde(default)]
    pub compaction_usage: Option<Usage>,
    pub execution_cost: Option<CostEstimate>,
    pub review_cost: Option<CostEstimate>,
    #[serde(default)]
    pub compaction_cost: Option<CostEstimate>,
}
/// An estimate in millionths of the explicitly configured currency; it is not a bill.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CostEstimate {
    pub currency: String,
    pub microunits: u64,
    pub rounded_up: bool,
    pub estimated: bool,
}
#[derive(Default)]
struct Totals {
    requests: u64,
    completed: u64,
    input: u64,
    output: u64,
    input_unknown: bool,
    output_unknown: bool,
    estimated: bool,
}
impl Totals {
    fn finish(&mut self, usage: Option<Usage>) {
        self.completed = self.completed.saturating_add(1);
        if let Some(usage) = usage {
            self.estimated |= usage.estimated;
            match usage.input_tokens {
                Some(n) => match self.input.checked_add(n) {
                    Some(sum) => self.input = sum,
                    None => self.input_unknown = true,
                },
                None => self.input_unknown = true,
            }
            match usage.output_tokens {
                Some(n) => match self.output.checked_add(n) {
                    Some(sum) => self.output = sum,
                    None => self.output_unknown = true,
                },
                None => self.output_unknown = true,
            }
        } else {
            self.input_unknown = true;
            self.output_unknown = true;
        }
    }
    fn usage(&self) -> Option<Usage> {
        if self.requests == 0 {
            return None;
        }
        let incomplete = self.completed != self.requests;
        Some(Usage {
            input_tokens: if self.input_unknown || incomplete {
                None
            } else {
                Some(self.input)
            },
            output_tokens: if self.output_unknown || incomplete {
                None
            } else {
                Some(self.output)
            },
            estimated: self.estimated || self.input_unknown || self.output_unknown || incomplete,
        })
    }
}
#[derive(Default)]
struct State {
    started: Option<Instant>,
    execution: Totals,
    review: Totals,
    compaction: Totals,
    tools: u64,
    executions: u64,
    calls: HashMap<String, String>,
    artifacts: Vec<Artifact>,
    execution_pricing: Option<Pricing>,
    review_pricing: Option<Pricing>,
}
#[derive(Default)]
pub(crate) struct Metrics {
    state: Mutex<State>,
}
impl Metrics {
    pub fn pricing(&self, execution: Option<Pricing>, review: Option<Pricing>) {
        if let Ok(mut state) = self.state.lock() {
            state.execution_pricing = execution;
            state.review_pricing = review;
        }
    }
    pub fn start(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.started = Some(Instant::now());
        }
    }
    pub fn tool_call(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.tools = state.tools.saturating_add(1);
        }
    }
    fn request(&self, purpose: ModelPurpose) {
        if let Ok(mut state) = self.state.lock() {
            let totals = match purpose {
                ModelPurpose::Execution => &mut state.execution,
                ModelPurpose::Review => &mut state.review,
                ModelPurpose::Compaction => &mut state.compaction,
            };
            totals.requests = totals.requests.saturating_add(1);
        }
    }
    fn finish(&self, purpose: ModelPurpose, usage: Option<Usage>) {
        if let Ok(mut state) = self.state.lock() {
            match purpose {
                ModelPurpose::Execution => &mut state.execution,
                ModelPurpose::Review => &mut state.review,
                ModelPurpose::Compaction => &mut state.compaction,
            }
            .finish(usage);
        }
    }
    pub fn observe(&self, changes: &[StoreChange]) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        for change in changes {
            match change {
                StoreChange::ToolIntent { call, .. } => {
                    state.calls.insert(call.call_id.clone(), call.name.clone());
                    if !changes.iter().any(|c|matches!(c,StoreChange::ToolFinished {call_id,..} if call_id==&call.call_id)) {state.executions=state.executions.saturating_add(1);}
                }
                StoreChange::ToolFinished {
                    call_id, result, ..
                } if result.ok
                    && state
                        .calls
                        .get(call_id)
                        .is_some_and(|name| name == "write_file" || name == "apply_patch")
                    && let Some(artifact) = artifact(result) =>
                {
                    state.artifacts.push(artifact);
                }
                _ => {}
            }
        }
    }
    pub fn snapshot(&self) -> Option<(RunMetrics, Option<Usage>, Vec<Artifact>)> {
        let state = self.state.lock().ok()?;
        let duration = state
            .started
            .map(|start| start.elapsed().as_millis())
            .unwrap_or(0);
        let execution = state.execution.usage();
        let review = state.review.usage();
        let compaction = state.compaction.usage();
        let usages: Vec<_> = [&execution, &review, &compaction]
            .into_iter()
            .flatten()
            .collect();
        let sum = |field: fn(&Usage) -> Option<u64>| {
            usages
                .iter()
                .try_fold(0u64, |total, usage| total.checked_add(field(usage)?))
        };
        let total = if usages.is_empty() {
            None
        } else {
            let input_tokens = sum(|usage| usage.input_tokens);
            let output_tokens = sum(|usage| usage.output_tokens);
            Some(Usage {
                input_tokens,
                output_tokens,
                estimated: usages.iter().any(|usage| usage.estimated)
                    || input_tokens.is_none()
                    || output_tokens.is_none(),
            })
        };
        Some((
            RunMetrics {
                execution_requests: state.execution.requests,
                review_requests: state.review.requests,
                compaction_requests: state.compaction.requests,
                tool_calls: state.tools,
                tool_executions: state.executions,
                duration_ms: u64::try_from(duration).unwrap_or(u64::MAX),
                execution_usage: execution,
                review_usage: review,
                compaction_usage: compaction,
                compaction_cost: estimate(
                    state.execution_pricing.as_ref(),
                    state.compaction.usage().as_ref(),
                ),
                execution_cost: estimate(
                    state.execution_pricing.as_ref(),
                    state.execution.usage().as_ref(),
                ),
                review_cost: estimate(state.review_pricing.as_ref(), state.review.usage().as_ref()),
            },
            total,
            state.artifacts.clone(),
        ))
    }
}
fn estimate(pricing: Option<&Pricing>, usage: Option<&Usage>) -> Option<CostEstimate> {
    let pricing = pricing?;
    let usage = usage?;
    let input = u128::from(usage.input_tokens?)
        .checked_mul(u128::from(pricing.input_microunits_per_million))?;
    let output = u128::from(usage.output_tokens?)
        .checked_mul(u128::from(pricing.output_microunits_per_million))?;
    let numerator = input.checked_add(output)?;
    let rounded_up = numerator % 1_000_000 != 0;
    let amount = (numerator / 1_000_000).checked_add(u128::from(rounded_up))?;
    Some(CostEstimate {
        currency: pricing.currency.clone(),
        microunits: u64::try_from(amount).ok()?,
        rounded_up,
        estimated: true,
    })
}
fn artifact(result: &ToolResult) -> Option<Artifact> {
    let data = result.data.as_ref()?;
    let path = data.get("path")?.as_str()?;
    let created = data.get("created")?.as_bool()?;
    Some(Artifact {
        path: path.into(),
        operation: if created { "created" } else { "modified" }.into(),
    })
}

pub(crate) struct MeasuredModel {
    pub inner: Arc<dyn Model>,
    pub metrics: Arc<Metrics>,
}
struct Completion {
    metrics: Arc<Metrics>,
    purpose: ModelPurpose,
    done: bool,
    span: tracing::Span,
    started: Instant,
    status: &'static str,
    error_code: Option<String>,
}
impl Drop for Completion {
    fn drop(&mut self) {
        if !self.done {
            self.metrics.finish(self.purpose, None);
        }
        self.span.in_scope(|| {
            tracing::info!(target: "saya::model", event = "model.finished", status = self.status,
                error_code = self.error_code.as_deref().unwrap_or(""), duration_ms = self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64);
        });
    }
}
struct MeasuredStream {
    inner: Box<dyn ModelStream>,
    completion: Completion,
}
impl Model for MeasuredModel {
    fn request_counter(&self) -> Option<&dyn crate::context_builder::RequestCounter> {
        self.inner.request_counter()
    }

    fn validate_session(&self, session: &Session) -> Result<(), BoundaryError> {
        self.inner.validate_session(session)
    }
    fn start<'a>(
        &'a self,
        request: &'a ModelRequest,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            self.metrics.request(request.purpose);
            let purpose = match request.purpose {
                ModelPurpose::Execution => "execution",
                ModelPurpose::Review => "review",
                ModelPurpose::Compaction => "compaction",
            };
            let span = tracing::info_span!(target: "saya::model", "model", request_id = request.request_id.as_str(), profile = request.profile.as_str(), purpose);
            span.in_scope(|| tracing::info!(target: "saya::model", event = "model.started"));
            let mut completion = Completion {
                metrics: self.metrics.clone(),
                purpose: request.purpose,
                done: false,
                span: span.clone(),
                started: Instant::now(),
                status: "abandoned",
                error_code: None,
            };
            let inner = self
                .inner
                .start(request, cancel)
                .instrument(span)
                .await
                .inspect_err(|error| {
                    completion.status = "failed";
                    completion.error_code = Some(error.0.code.clone());
                })?;
            Ok(Box::new(MeasuredStream { inner, completion }) as Box<dyn ModelStream>)
        })
    }
}
impl ModelStream for MeasuredStream {
    fn next<'a>(&'a mut self, cancel: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
        Box::pin(async move {
            let next = self
                .inner
                .next(cancel)
                .instrument(self.completion.span.clone())
                .await
                .inspect_err(|error| {
                    self.completion.status = "failed";
                    self.completion.error_code = Some(error.0.code.clone());
                })?;
            if let Some(ModelUpdate::Finished { usage, .. }) = &next
                && !self.completion.done
            {
                self.completion
                    .metrics
                    .finish(self.completion.purpose, usage.clone());
                self.completion.done = true;
                self.completion.status = "completed";
            }
            Ok(next)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pricing_uses_explicit_rates_and_never_guesses_missing_usage() {
        let pricing = Pricing {
            currency: "USD".into(),
            input_microunits_per_million: 1,
            output_microunits_per_million: 1,
        };
        let usage = Usage {
            input_tokens: Some(1),
            output_tokens: Some(1),
            estimated: false,
        };
        let cost = estimate(Some(&pricing), Some(&usage)).unwrap();
        assert_eq!(cost.microunits, 1);
        assert!(cost.rounded_up);
        assert!(cost.estimated);
        assert!(estimate(None, Some(&usage)).is_none());
        assert!(
            estimate(
                Some(&pricing),
                Some(&Usage {
                    input_tokens: None,
                    ..usage.clone()
                })
            )
            .is_none()
        );
        let large = Pricing {
            input_microunits_per_million: u64::MAX,
            output_microunits_per_million: u64::MAX,
            ..pricing
        };
        assert!(
            estimate(
                Some(&large),
                Some(&Usage {
                    input_tokens: Some(u64::MAX),
                    output_tokens: Some(u64::MAX),
                    estimated: false
                })
            )
            .is_none()
        );
    }
}
