//! Test-only runtime budgets and deterministic retry decisions.
use std::time::Duration;
use tokio::time::Instant;

#[derive(Debug, Clone)]
pub(super) struct Limits {
    pub model_requests: usize,
    pub tool_calls: usize,
    pub active_time: Duration,
    pub model_connect: Duration,
    pub model_idle: Duration,
    pub model_total: Duration,
    pub tool_timeout: Duration,
    pub retry_jitter_max: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            model_requests: 64,
            tool_calls: 256,
            active_time: Duration::from_secs(1800),
            model_connect: Duration::from_secs(10),
            model_idle: Duration::from_secs(60),
            model_total: Duration::from_secs(180),
            tool_timeout: Duration::from_secs(120),
            retry_jitter_max: Duration::from_millis(250),
        }
    }
}

pub(super) struct Budget {
    limits: Limits,
    started: Instant,
    paused_at: Option<Instant>,
    paused_total: Duration,
    requests: usize,
    tools: usize,
}
impl Budget {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            started: Instant::now(),
            paused_at: None,
            paused_total: Duration::ZERO,
            requests: 0,
            tools: 0,
        }
    }
    pub fn remaining(&self) -> Duration {
        let now = self.paused_at.unwrap_or_else(Instant::now);
        self.limits.active_time.saturating_sub(
            now.duration_since(self.started)
                .saturating_sub(self.paused_total),
        )
    }
    pub fn request(&mut self) -> bool {
        if self.requests >= self.limits.model_requests || self.remaining().is_zero() {
            return false;
        }
        self.requests += 1;
        true
    }
    pub fn tool(&mut self) -> bool {
        if self.tools >= self.limits.tool_calls || self.remaining().is_zero() {
            return false;
        }
        self.tools += 1;
        true
    }
    pub fn pause(&mut self) {
        if self.paused_at.is_none() {
            self.paused_at = Some(Instant::now());
        }
    }
    pub fn resume(&mut self) {
        if let Some(since) = self.paused_at.take() {
            self.paused_total += since.elapsed();
        }
    }
}

/// Only pre-response transport failures, rate limits and retryable servers qualify.
pub(super) fn retry_delay(
    code: &str,
    attempt: usize,
    partial: bool,
    retry_after: Option<Duration>,
    jitter: Duration,
) -> Option<Duration> {
    if partial
        || attempt >= 2
        || !matches!(
            code,
            "transient_connection" | "rate_limited" | "server_error"
        )
    {
        return None;
    }
    let backoff = Duration::from_secs(1 << attempt) + jitter;
    Some(backoff.max(retry_after.unwrap_or_default().min(Duration::from_secs(30))))
}
