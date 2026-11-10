//! Interface-only turn measurements taken before events enter the UI queue.
use crate::i18n::summary as text;
use saya_app::events::{ChannelSink, EventSink};
use saya_core::{
    boundary::Operation,
    contract::{Event, EventPayload, ModelPurpose, Usage},
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::mpsc, time::Instant};

pub(super) struct TurnSummary {
    started: Instant,
    model: String,
    measurements: Arc<Mutex<Measurements>>,
}

#[derive(Default)]
struct Measurements {
    pending: Option<(String, Instant)>,
    requests: u64,
    output_tokens: u64,
    elapsed: Duration,
    estimated: bool,
    incomplete: bool,
}

impl Measurements {
    fn observe(&mut self, payload: &EventPayload, now: Instant) {
        match payload {
            EventPayload::ModelStarted {
                request_id,
                purpose: ModelPurpose::Execution,
                ..
            } => {
                if self.pending.is_some() || request_id.len() > 256 {
                    self.incomplete = true;
                    self.pending = None;
                    return;
                }
                self.requests = self.requests.saturating_add(1);
                self.pending = Some((request_id.clone(), now));
            }
            EventPayload::ModelFinished {
                request_id,
                purpose: ModelPurpose::Execution,
                usage,
                ..
            } => {
                let Some((pending_id, started)) = self.pending.take() else {
                    self.incomplete = true;
                    return;
                };
                let duration = now.checked_duration_since(started);
                let output = usage.as_ref().and_then(|usage| usage.output_tokens);
                let elapsed = duration.and_then(|duration| self.elapsed.checked_add(duration));
                let tokens = output.and_then(|output| self.output_tokens.checked_add(output));
                if pending_id != *request_id || elapsed.is_none() || tokens.is_none() {
                    self.incomplete = true;
                    return;
                }
                if let (Some(elapsed), Some(tokens)) = (elapsed, tokens) {
                    self.elapsed = elapsed;
                    self.output_tokens = tokens;
                    self.estimated |= usage.as_ref().is_some_and(|usage| usage.estimated);
                }
            }
            _ => {}
        }
    }

    fn rate(&self) -> Option<(f64, bool)> {
        if self.incomplete || self.pending.is_some() || self.requests == 0 || self.elapsed.is_zero()
        {
            return None;
        }
        let rate = self.output_tokens as f64 / self.elapsed.as_secs_f64();
        rate.is_finite().then_some((rate, self.estimated))
    }
}

pub(super) struct SummarySink {
    channel: ChannelSink,
    measurements: Arc<Mutex<Measurements>>,
}

impl EventSink for SummarySink {
    fn publish<'a>(&'a self, event: &'a Event) -> Operation<'a, ()> {
        if let Ok(mut measurements) = self.measurements.lock() {
            measurements.observe(&event.payload, Instant::now());
        }
        self.channel.publish(event)
    }
}

impl TurnSummary {
    pub(super) fn new(model: String) -> Self {
        Self {
            started: Instant::now(),
            model,
            measurements: Arc::new(Mutex::new(Measurements::default())),
        }
    }

    pub(super) fn sink(&self, sender: mpsc::Sender<Event>) -> SummarySink {
        SummarySink {
            channel: ChannelSink(sender),
            measurements: self.measurements.clone(),
        }
    }

    pub(super) fn start(&mut self, now: Instant) {
        self.started = now;
    }

    pub(super) fn elapsed_at(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.started)
    }

    pub(super) fn text(&self, usage: Option<&Usage>) -> String {
        self.text_at(usage, Instant::now())
    }

    fn text_at(&self, usage: Option<&Usage>, now: Instant) -> String {
        let elapsed = text::duration(self.elapsed_at(now));
        let rate = self
            .measurements
            .lock()
            .ok()
            .and_then(|measurements| measurements.rate())
            .map(|(rate, estimated)| text::rate(rate, estimated))
            .unwrap_or_else(text::no_rate);
        let tokens = usage
            .and_then(|usage| {
                usage
                    .input_tokens?
                    .checked_add(usage.output_tokens?)
                    .map(|total| text::tokens(total, usage.estimated))
            })
            .unwrap_or_else(text::no_tokens);
        format!("{} · {elapsed} · {rate} · {tokens}", self.model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use saya_core::contract::{SessionId, WorkMode};

    fn started(id: &str, purpose: ModelPurpose) -> EventPayload {
        EventPayload::ModelStarted {
            request_id: id.into(),
            purpose,
            profile: "fixture".into(),
        }
    }

    fn finished(
        id: &str,
        purpose: ModelPurpose,
        output: Option<u64>,
        estimated: bool,
    ) -> EventPayload {
        EventPayload::ModelFinished {
            request_id: id.into(),
            purpose,
            ok: true,
            usage: Some(Usage {
                input_tokens: Some(100),
                output_tokens: output,
                estimated,
            }),
            error: None,
        }
    }

    #[test]
    fn producer_model_intervals_exclude_tool_review_and_delayed_ui_drain_time() {
        let summary = TurnSummary::new("fixture-model".into());
        let now = summary.started;
        let mut measurements = summary.measurements.lock().unwrap();
        for (payload, seconds) in [
            (started("a", ModelPurpose::Execution), 0),
            (finished("a", ModelPurpose::Execution, Some(200), false), 2),
            (started("review", ModelPurpose::Review), 3),
            (
                finished("review", ModelPurpose::Review, Some(9999), false),
                19,
            ),
            (started("b", ModelPurpose::Execution), 20),
            (finished("b", ModelPurpose::Execution, Some(300), false), 23),
        ] {
            measurements.observe(&payload, now + Duration::from_secs(seconds));
        }
        drop(measurements);
        let text = summary.text_at(
            Some(&Usage {
                input_tokens: Some(9000),
                output_tokens: Some(2000),
                estimated: false,
            }),
            now + Duration::from_secs(79),
        );
        assert_eq!(text, "fixture-model · 1m 19s · 100.0 tok/s · 11k tokens");
    }

    #[test]
    fn missing_partial_overflowed_and_estimated_usage_keep_their_meaning() {
        let summary = TurnSummary::new("fixture-model".into());
        let now = summary.started;
        for usage in [
            None,
            Some(Usage {
                input_tokens: None,
                output_tokens: Some(3),
                estimated: false,
            }),
            Some(Usage {
                input_tokens: Some(3),
                output_tokens: None,
                estimated: false,
            }),
            Some(Usage {
                input_tokens: Some(u64::MAX),
                output_tokens: Some(1),
                estimated: false,
            }),
        ] {
            assert_eq!(
                summary.text_at(usage.as_ref(), now),
                "fixture-model · 0.0s · — tok/s · — tokens"
            );
        }
        let mut measurements = summary.measurements.lock().unwrap();
        measurements.observe(&started("a", ModelPurpose::Execution), now);
        measurements.observe(
            &finished("a", ModelPurpose::Execution, Some(400), true),
            now + Duration::from_secs(2),
        );
        drop(measurements);
        assert_eq!(
            summary.text_at(
                Some(&Usage {
                    input_tokens: Some(1000),
                    output_tokens: Some(200),
                    estimated: true
                }),
                now + Duration::from_secs(2)
            ),
            "fixture-model · 2.0s · ~200.0 tok/s · ~1.2k tokens"
        );
    }

    #[test]
    fn incomplete_unmatched_zero_duration_and_overflowed_model_counts_have_no_rate() {
        let now = Instant::now();
        for bad_finish in [
            finished("other", ModelPurpose::Execution, Some(10), false),
            finished("a", ModelPurpose::Execution, None, false),
        ] {
            let mut measurements = Measurements::default();
            measurements.observe(&started("a", ModelPurpose::Execution), now);
            measurements.observe(&bad_finish, now + Duration::from_secs(1));
            assert!(measurements.rate().is_none());
        }
        let mut measurements = Measurements::default();
        measurements.observe(&started("a", ModelPurpose::Execution), now);
        assert!(measurements.rate().is_none());
        measurements.observe(
            &finished("a", ModelPurpose::Execution, Some(u64::MAX), false),
            now,
        );
        assert!(measurements.rate().is_none());
        measurements.observe(&started("b", ModelPurpose::Execution), now);
        measurements.observe(
            &finished("b", ModelPurpose::Execution, Some(1), false),
            now + Duration::from_secs(1),
        );
        assert!(measurements.rate().is_none());
    }

    #[tokio::test]
    async fn summary_sink_forwards_exact_events_and_preserves_delivery_errors() {
        let summary = TurnSummary::new("fixture-model".into());
        let (sender, mut receiver) = mpsc::channel(1);
        let sink = summary.sink(sender);
        let event = Event {
            schema_version: 1,
            session_id: SessionId::new(),
            run_id: None,
            seq: 1,
            timestamp: "fixture".into(),
            payload: started("a", ModelPurpose::Execution),
        };
        sink.publish(&event).await.unwrap();
        assert_eq!(receiver.recv().await.unwrap(), event);
        assert!(summary.measurements.lock().unwrap().pending.is_some());
        drop(receiver);
        let mut closed = event;
        closed.payload = EventPayload::RunStarted {
            work_mode: WorkMode::Work,
            permissions: saya_core::contract::PermissionMode::Auto,
            profile: "fixture".into(),
        };
        assert_eq!(
            sink.publish(&closed).await.unwrap_err().0.code,
            "event_consumer_closed"
        );
    }
}
