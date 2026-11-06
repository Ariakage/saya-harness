//! Independent model review for operations selected by the permission policy.
//! This component never executes tools and does not grant persistent approvals.

use crate::{
    boundary::{Cancellation, Model, ModelRequest, ModelUpdate},
    contract::{BoundaryError, ErrorInfo, HistoryItem, ModelPurpose, PermissionMode, WorkMode},
    permissions::{Decision, Operation, Policy},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::{Instant, sleep};

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

struct BudgetState {
    count: usize,
    started: Instant,
    paused_at: Option<Instant>,
    paused: Duration,
}
/// Shared per-run model request and active-time budget. Execution and review
/// callers must use the same instance; each attempt consumes one request.
#[derive(Clone)]
pub struct SharedBudget {
    state: Arc<Mutex<BudgetState>>,
    max_requests: usize,
    active_limit: Duration,
}
impl SharedBudget {
    /// Start timing when a newly constructed runtime actually begins its run.
    pub(crate) fn start_run(&self) -> Result<(), BoundaryError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| failure("budget_error", "budget lock poisoned"))?;
        if state.count != 0 || state.paused_at.is_some() {
            return Err(failure("budget_error", "budget has already been used"));
        }
        state.started = Instant::now();
        state.paused = Duration::ZERO;
        Ok(())
    }
    pub fn new(max_requests: usize, active_limit: Duration) -> Self {
        Self {
            state: Arc::new(Mutex::new(BudgetState {
                count: 0,
                started: Instant::now(),
                paused_at: None,
                paused: Duration::ZERO,
            })),
            max_requests,
            active_limit,
        }
    }
    /// Consume one request, returning a limit error when exhausted.
    pub fn consume(&self) -> Result<(), BoundaryError> {
        let mut s = self
            .state
            .lock()
            .map_err(|_| failure("budget_error", "budget lock poisoned"))?;
        if s.paused_at.is_some() {
            return Err(failure(
                "budget_error",
                "model request while active time is paused",
            ));
        }
        let now = Instant::now();
        if s.count >= self.max_requests
            || now.duration_since(s.started).saturating_sub(s.paused) >= self.active_limit
        {
            return Err(failure(
                "limit_reached",
                "model request or active time budget exceeded",
            ));
        }
        s.count += 1;
        Ok(())
    }
    pub fn used(&self) -> Result<usize, BoundaryError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| failure("budget_error", "budget lock poisoned"))?
            .count)
    }
    pub fn remaining(&self) -> Result<Duration, BoundaryError> {
        let s = self
            .state
            .lock()
            .map_err(|_| failure("budget_error", "budget lock poisoned"))?;
        let now = s.paused_at.unwrap_or_else(Instant::now);
        Ok(self
            .active_limit
            .saturating_sub(now.duration_since(s.started).saturating_sub(s.paused)))
    }
    /// Pause active time during a human confirmation or clarification.
    pub fn pause(&self) -> Result<(), BoundaryError> {
        let mut s = self
            .state
            .lock()
            .map_err(|_| failure("budget_error", "budget lock poisoned"))?;
        if s.paused_at.is_none() {
            s.paused_at = Some(Instant::now());
        }
        Ok(())
    }
    /// Resume active time after human input.
    pub fn resume(&self) -> Result<(), BoundaryError> {
        let mut s = self
            .state
            .lock()
            .map_err(|_| failure("budget_error", "budget lock poisoned"))?;
        if let Some(since) = s.paused_at.take() {
            s.paused += since.elapsed();
        }
        Ok(())
    }
}
impl Default for SharedBudget {
    fn default() -> Self {
        Self::new(64, Duration::from_secs(1800))
    }
}

/// Runtime-controlled review settings; credentials must never be placed in a
/// request. `known_secrets` are redacted from the data and model reason.
#[derive(Clone)]
pub struct ReviewConfig {
    pub execution_profile: String,
    pub review_profile: Option<String>,
    pub max_output_tokens: u32,
    pub connect_timeout: Duration,
    pub idle_timeout: Duration,
    pub total_timeout: Duration,
    pub known_secrets: Vec<String>,
    pub retry_jitter_max: Duration,
}
impl ReviewConfig {
    pub fn new(execution_profile: String) -> Self {
        Self {
            execution_profile,
            review_profile: None,
            max_output_tokens: 1024,
            connect_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(60),
            total_timeout: Duration::from_secs(180),
            known_secrets: vec![],
            retry_jitter_max: Duration::from_millis(250),
        }
    }
}
/// Where a decision was made. HumanFallback means a user confirmation is needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewDecisionSource {
    Rule,
    Model,
    HumanFallback,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewOutcome {
    pub decision: Decision,
    pub reason: String,
    pub source: ReviewDecisionSource,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelDecision {
    decision: ModelChoice,
    reason: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ModelChoice {
    Allow,
    Ask,
    Deny,
}

fn redact(value: &mut Value, secrets: &[String]) {
    match value {
        Value::String(text) => {
            for secret in secrets.iter().filter(|s| !s.is_empty()) {
                *text = text.replace(secret, "[REDACTED]");
            }
        }
        Value::Array(items) => {
            for item in items {
                redact(item, secrets);
            }
        }
        Value::Object(map) => {
            let old = std::mem::take(map);
            for (mut key, mut value) in old {
                for secret in secrets.iter().filter(|s| !s.is_empty()) {
                    key = key.replace(secret, "[REDACTED]");
                }
                redact(&mut value, secrets);
                map.insert(key, value);
            }
        }
        _ => {}
    }
}
fn jitter(max: Duration) -> Duration {
    let millis = max.as_millis().min(u128::from(u64::MAX - 1)) as u64;
    if millis == 0 {
        Duration::ZERO
    } else {
        Duration::from_millis((uuid::Uuid::now_v7().as_u128() as u64) % (millis + 1))
    }
}
async fn bounded<T>(
    future: impl Future<Output = Result<T, BoundaryError>>,
    cancel: &dyn Cancellation,
    budget: &SharedBudget,
    duration: Duration,
) -> Result<T, BoundaryError> {
    if cancel.is_cancelled() {
        return Err(failure("cancelled", "review cancelled"));
    }
    let remaining = budget.remaining()?;
    if remaining.is_zero() {
        return Err(failure("limit_reached", "active time budget exceeded"));
    }
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(failure("cancelled", "review cancelled")),
        _ = sleep(remaining) => Err(failure("limit_reached", "active time budget exceeded")),
        _ = sleep(duration) => Err(failure("model_timeout", "review request timed out")),
        result = future => result,
    }
}

/// Data for a single independent review request. Tool arguments are data, not instructions.
pub struct ReviewInput<'a> {
    pub mode: WorkMode,
    pub permissions: PermissionMode,
    pub operation: &'a Operation,
    pub arguments: Value,
    pub goal_summary: &'a str,
}

/// Evaluate the policy, then only when required call a separate, tool-free model
/// request. Malformed/failed reviews fall back to `Ask`; cancellation and budget
/// exhaustion are returned as errors and must stop dispatch.
pub async fn review_operation(
    policy: &Policy,
    input: ReviewInput<'_>,
    model: &dyn Model,
    budget: &SharedBudget,
    config: &ReviewConfig,
    cancel: &dyn Cancellation,
) -> Result<ReviewOutcome, BoundaryError> {
    if cancel.is_cancelled() {
        return Err(failure("cancelled", "review cancelled"));
    }
    if config.max_output_tokens == 0 {
        return Err(failure(
            "invalid_config",
            "review output token limit must be positive",
        ));
    }
    let initial = policy.evaluate(input.mode, input.permissions, input.operation);
    if initial.decision != Decision::ModelReview {
        return Ok(ReviewOutcome {
            decision: initial.decision,
            reason: initial.reason.into(),
            source: ReviewDecisionSource::Rule,
        });
    }
    let normalized = input.operation.normalized_arguments(input.arguments)?;
    // If an operation contains a known secret, do not ask the model to approve
    // an incomplete redacted view; fall back to a human decision.
    fn contains_secret(value: &Value, secrets: &[String]) -> bool {
        match value {
            Value::String(text) => secrets
                .iter()
                .any(|secret| !secret.is_empty() && text.contains(secret)),
            Value::Array(items) => items.iter().any(|item| contains_secret(item, secrets)),
            Value::Object(map) => map.iter().any(|(key, value)| {
                secrets
                    .iter()
                    .any(|secret| !secret.is_empty() && key.contains(secret))
                    || contains_secret(value, secrets)
            }),
            _ => false,
        }
    }
    if contains_secret(&normalized, &config.known_secrets) {
        return Ok(ReviewOutcome {
            decision: Decision::Ask,
            reason: "review_sensitive_data".into(),
            source: ReviewDecisionSource::HumanFallback,
        });
    }
    let mut data = json!({"tool": input.operation.tool_name(), "arguments": normalized, "work_mode": input.mode, "permissions": input.permissions, "goal_summary": input.goal_summary});
    redact(&mut data, &config.known_secrets);
    let profile = config
        .review_profile
        .as_ref()
        .unwrap_or(&config.execution_profile);
    let instructions = crate::prompt::review_instructions();
    for attempt in 0..=2usize {
        if cancel.is_cancelled() {
            return Err(failure("cancelled", "review cancelled"));
        }
        budget.consume()?;
        let request = ModelRequest {
            request_id: uuid::Uuid::now_v7().to_string(),
            purpose: ModelPurpose::Review,
            profile: profile.clone(),
            instructions: instructions.into(),
            max_output_tokens: Some(config.max_output_tokens),
            history: vec![HistoryItem::UserText(data.to_string())],
            tools: vec![],
        };
        let started = Instant::now();
        let mut received = false;
        let result: Result<Option<String>, BoundaryError> = async {
            let mut stream = bounded(
                model.start(&request, cancel),
                cancel,
                budget,
                config.connect_timeout.min(config.total_timeout),
            )
            .await?;
            loop {
                let remaining = config.total_timeout.saturating_sub(started.elapsed());
                match bounded(
                    stream.next(cancel),
                    cancel,
                    budget,
                    config.idle_timeout.min(remaining),
                )
                .await?
                {
                    Some(ModelUpdate::TextDelta(_)) => {
                        received = true;
                    }
                    Some(ModelUpdate::Finished { items, .. }) => {
                        let mut text = String::new();
                        for item in items {
                            match item {
                                HistoryItem::AssistantText(s) => {
                                    if text.len().saturating_add(s.len()) > 64 * 1024 {
                                        return Ok(None);
                                    }
                                    text.push_str(&s);
                                }
                                HistoryItem::ProviderItem { .. } => {}
                                _ => return Ok(None),
                            }
                        }
                        return Ok(Some(text));
                    }
                    None => return Ok(None),
                }
            }
        }
        .await;
        match result {
            Ok(Some(text)) => {
                if let Ok(parsed) = serde_json::from_str::<ModelDecision>(&text)
                    && !parsed.reason.trim().is_empty()
                    && parsed.reason.chars().count() <= 512
                {
                    let mut reason = Value::String(parsed.reason);
                    redact(&mut reason, &config.known_secrets);
                    let reason = reason.as_str().unwrap_or_default().to_owned();
                    let decision = match parsed.decision {
                        ModelChoice::Allow => Decision::Allow,
                        ModelChoice::Ask => Decision::Ask,
                        ModelChoice::Deny => Decision::Deny,
                    };
                    return Ok(ReviewOutcome {
                        decision,
                        reason,
                        source: ReviewDecisionSource::Model,
                    });
                }
                return Ok(ReviewOutcome {
                    decision: Decision::Ask,
                    reason: "review_invalid".into(),
                    source: ReviewDecisionSource::HumanFallback,
                });
            }
            Ok(None) => {
                return Ok(ReviewOutcome {
                    decision: Decision::Ask,
                    reason: "review_invalid".into(),
                    source: ReviewDecisionSource::HumanFallback,
                });
            }
            Err(error) => {
                if matches!(
                    error.0.code.as_str(),
                    "cancelled" | "limit_reached" | "budget_error"
                ) {
                    return Err(error);
                }
                let retryable = matches!(
                    error.0.code.as_str(),
                    "transient_connection" | "rate_limited" | "server_error"
                );
                if !retryable || received || attempt == 2 {
                    return Ok(ReviewOutcome {
                        decision: Decision::Ask,
                        reason: "review_unavailable".into(),
                        source: ReviewDecisionSource::HumanFallback,
                    });
                }
                let delay = (Duration::from_secs(1 << attempt) + jitter(config.retry_jitter_max))
                    .max(error.1.unwrap_or_default().min(Duration::from_secs(30)));
                bounded(
                    async {
                        sleep(delay).await;
                        Ok(())
                    },
                    cancel,
                    budget,
                    budget.remaining()?.saturating_add(Duration::from_secs(1)),
                )
                .await?;
            }
        }
    }
    Ok(ReviewOutcome {
        decision: Decision::Ask,
        reason: "review_unavailable".into(),
        source: ReviewDecisionSource::HumanFallback,
    })
}
