use super::{
    CompactionConfig, CompactionInput, CompactionStage, MAX_SUMMARY_BYTES, StructuredSummary,
    SummaryCallRecord, ensure_no_secrets, error,
    planning::{self, Plan, Unit},
    schema,
};
use crate::{
    boundary::{Cancellation, Model, ModelRequest, ModelUpdate},
    context_builder::{self, ItemRange, RequestCounter},
    contract::{BoundaryError, HistoryItem, Usage},
    review::SharedBudget,
};
use std::{future::Future, time::Duration};
use tokio::time::{Instant, sleep, sleep_until};

pub(super) async fn bounded<T>(
    future: impl Future<Output = Result<T, BoundaryError>>,
    cancel: &dyn Cancellation,
    budget: &SharedBudget,
    deadline: Instant,
) -> Result<T, BoundaryError> {
    if cancel.is_cancelled() {
        return Err(error("cancelled", "Compaction was cancelled"));
    }
    let remaining = budget.remaining()?;
    if remaining.is_zero() {
        return Err(error(
            "limit_reached",
            "Shared active-time budget is exhausted",
        ));
    }
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(error("cancelled", "Compaction was cancelled")),
        _ = sleep(remaining) => Err(error("limit_reached", "Shared active-time budget is exhausted")),
        _ = sleep_until(deadline) => Err(error("model_timeout", "Compaction exceeded its time limit")),
        result = future => result,
    }
}

pub(super) struct Runner<'a, 'source> {
    pub model: &'a dyn Model,
    pub budget: &'a SharedBudget,
    pub counter: Option<&'a dyn RequestCounter>,
    pub cancel: &'a dyn Cancellation,
    pub config: &'a CompactionConfig,
    pub input: &'a CompactionInput<'source>,
    pub deadline: Instant,
    pub calls: Vec<SummaryCallRecord>,
}

struct AttemptOutcome {
    result: Result<String, BoundaryError>,
    usage: Option<Usage>,
    received: bool,
}

impl Runner<'_, '_> {
    pub async fn summarize(
        &mut self,
        units: Vec<Unit>,
        plan: &Plan,
        stage: CompactionStage,
    ) -> Result<StructuredSummary, BoundaryError> {
        let mut request =
            planning::request_for(&units, plan, self.input, self.config, stage.clone())?;
        let range = ItemRange {
            start: units
                .first()
                .ok_or_else(|| error("compaction_invalid", "Summary input is empty"))?
                .range
                .start,
            end: units
                .last()
                .ok_or_else(|| error("compaction_invalid", "Summary input is empty"))?
                .range
                .end,
        };
        for attempt in 0..=self.config.max_retries {
            if self.cancel.is_cancelled() {
                return Err(error("cancelled", "Compaction was cancelled"));
            }
            if self.calls.len() >= self.config.max_model_calls {
                return Err(error(
                    "limit_reached",
                    "Compaction model-call limit reached",
                ));
            }
            if Instant::now() >= self.deadline {
                return Err(error(
                    "model_timeout",
                    "Compaction operation deadline reached",
                ));
            }
            request.request_id = uuid::Uuid::now_v7().to_string();
            request = planning::fitted_request(&request, self.input, self.config, self.counter)?
                .ok_or_else(|| {
                    error(
                        "context_limit",
                        "Summary request no longer fits its input capacity",
                    )
                })?;
            let fingerprint =
                context_builder::request_fingerprint(&request, self.input.context.identity)?;
            // Exactly one shared request is consumed for every actual start.
            self.model.check_start(&request)?;
            self.budget.consume()?;
            let started = Instant::now();
            let outcome = self.attempt(&request).await;
            self.calls.push(SummaryCallRecord {
                request_id: request.request_id.clone(),
                stage: stage.clone(),
                attempt,
                request_fingerprint: fingerprint,
                source_range: range,
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                usage: outcome.usage,
                outcome: outcome
                    .result
                    .as_ref()
                    .map_or_else(|error| error.0.code.clone(), |_| "completed".into()),
            });
            match outcome.result {
                Ok(text) => {
                    ensure_no_secrets(text.as_bytes(), self.input.known_secrets)?;
                    let summary: StructuredSummary = serde_json::from_str(&text).map_err(|_| {
                        error(
                            "compaction_invalid",
                            "Summary output is not the exact structured JSON contract",
                        )
                    })?;
                    schema::validate(&summary, &plan.validation_for(range))?;
                    return Ok(summary);
                }
                Err(failure) => {
                    let retryable = matches!(
                        failure.0.code.as_str(),
                        "transient_connection" | "rate_limited" | "server_error"
                    );
                    if !retryable || outcome.received || attempt == self.config.max_retries {
                        return Err(failure);
                    }
                    let jitter_cap = self.config.retry_jitter_max.as_millis() as u64;
                    let jitter = if jitter_cap == 0 {
                        Duration::ZERO
                    } else {
                        Duration::from_millis(
                            (uuid::Uuid::now_v7().as_u128() as u64) % (jitter_cap + 1),
                        )
                    };
                    let delay = (Duration::from_secs(1 << attempt) + jitter)
                        .max(failure.1.unwrap_or_default().min(Duration::from_secs(30)));
                    bounded(
                        async {
                            sleep(delay).await;
                            Ok(())
                        },
                        self.cancel,
                        self.budget,
                        self.deadline,
                    )
                    .await?;
                }
            }
        }
        Err(error("compaction_invalid", "Summary retry limit reached"))
    }

    async fn attempt(&self, request: &ModelRequest) -> AttemptOutcome {
        let request_deadline = self
            .deadline
            .min(Instant::now() + self.config.request_timeout);
        let mut received = false;
        let mut usage = None;
        let result = async {
            let mut stream = bounded(
                self.model.start(request, self.cancel),
                self.cancel,
                self.budget,
                request_deadline.min(Instant::now() + self.config.connect_timeout),
            )
            .await?;
            let mut deltas = String::new();
            let mut saw_text_delta = false;
            loop {
                let update = bounded(
                    stream.next(self.cancel),
                    self.cancel,
                    self.budget,
                    request_deadline.min(Instant::now() + self.config.idle_timeout),
                )
                .await?;
                match update {
                    Some(ModelUpdate::TextDelta(text)) => {
                        received = true;
                        saw_text_delta = true;
                        if deltas.len().saturating_add(text.len()) > MAX_SUMMARY_BYTES {
                            return Err(error(
                                "compaction_invalid",
                                "Summary stream exceeds its text limit",
                            ));
                        }
                        deltas.push_str(&text);
                    }
                    Some(ModelUpdate::Progress | ModelUpdate::ThinkingDelta { .. }) => {
                        received = true
                    }
                    Some(ModelUpdate::Finished {
                        items,
                        usage: completed_usage,
                    }) => {
                        received = true;
                        usage = completed_usage;
                        let text = finished_text(self.input.context.identity.protocol, &items)?;
                        if saw_text_delta && deltas != text {
                            return Err(error(
                                "compaction_invalid",
                                "Summary stream differs from its completed text",
                            ));
                        }
                        return Ok(text);
                    }
                    None => {
                        return Err(error(
                            "compaction_invalid",
                            "Summary stream ended without a completed response",
                        ));
                    }
                }
            }
        }
        .await;
        // Adapter messages and partial bodies never become compaction errors.
        let result = result.map_err(|failure: BoundaryError| {
            let code = if schema::identifier(&failure.0.code)
                && !crate::redaction::contains_secret(&failure.0.code, self.input.known_secrets)
            {
                failure.0.code
            } else {
                "model_error".into()
            };
            BoundaryError(
                crate::contract::ErrorInfo {
                    code,
                    message: "Compaction model request failed".into(),
                },
                failure.1,
                None,
            )
        });
        AttemptOutcome {
            result,
            usage,
            received,
        }
    }
}

fn finished_text(
    protocol: crate::contract::ModelProtocol,
    items: &[HistoryItem],
) -> Result<String, BoundaryError> {
    if items.iter().any(|item| {
        !matches!(
            item,
            HistoryItem::AssistantText(_) | HistoryItem::ProviderItem { .. }
        )
    }) {
        return Err(error(
            "compaction_invalid",
            "Summary response contains a tool call or unsupported role",
        ));
    }
    context_builder::group_history(protocol, items).map_err(|_| {
        error(
            "compaction_invalid",
            "Summary provider state is not a complete valid response",
        )
    })?;
    let mut text = String::new();
    for item in items {
        if let HistoryItem::AssistantText(value) = item {
            if text.len().saturating_add(value.len()) > MAX_SUMMARY_BYTES {
                return Err(error(
                    "compaction_invalid",
                    "Summary output exceeds its text limit",
                ));
            }
            text.push_str(value);
        }
    }
    if text.is_empty() {
        return Err(error(
            "compaction_invalid",
            "Summary response contains no structured text",
        ));
    }
    Ok(text)
}
