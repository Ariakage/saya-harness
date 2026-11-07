//! Bounded, interface-owned plaintext thinking. Opaque provider data stays private.
use saya_core::{
    boundary::{Cancellation, Model, ModelRequest, ModelStream, ModelUpdate, Operation},
    contract::{BoundaryError, HistoryItem, ModelProtocol, ModelPurpose, Session},
};
use serde_json::Value;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::watch;

const MAX_ENTRIES: usize = 64;
const MAX_ENTRY_BYTES: usize = 256 * 1024;
const MAX_LOG_BYTES: usize = 2 * 1024 * 1024;
const MAX_RAW_BYTES: usize = 16 * 1024 * 1024;
const PUBLISH_INTERVAL: Duration = Duration::from_millis(33);

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ThinkingLog {
    pub entries: Vec<ThinkingSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThinkingSnapshot {
    pub request_id: String,
    pub text: String,
    pub active: bool,
    pub truncated: bool,
    pub available: bool,
}

/// Observe execution thinking without changing requests, events or stored items.
/// Review thinking is consumed as progress and is never published to this log.
pub struct ThinkingModel {
    inner: Arc<dyn Model>,
    sender: watch::Sender<ThinkingLog>,
    secrets: Arc<Vec<String>>,
}
impl ThinkingModel {
    pub fn new(
        inner: Arc<dyn Model>,
        sender: watch::Sender<ThinkingLog>,
        known_secrets: Vec<String>,
    ) -> Self {
        Self {
            inner,
            sender,
            secrets: Arc::new(known_secrets),
        }
    }
}
impl Model for ThinkingModel {
    fn validate_session(&self, session: &Session) -> Result<(), BoundaryError> {
        self.inner.validate_session(session)
    }
    fn start<'a>(
        &'a self,
        request: &'a ModelRequest,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            let capture = (request.purpose == ModelPurpose::Execution).then(|| {
                Capture::new(
                    request.request_id.clone(),
                    self.sender.clone(),
                    self.secrets.clone(),
                )
            });
            let inner = self.inner.start(request, cancel).await?;
            Ok(Box::new(ThinkingStream { inner, capture }) as Box<dyn ModelStream>)
        })
    }
}
struct ThinkingStream {
    inner: Box<dyn ModelStream>,
    capture: Option<Capture>,
}
impl ModelStream for ThinkingStream {
    fn next<'a>(&'a mut self, cancel: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
        Box::pin(async move {
            let result = self.inner.next(cancel).await;
            match &result {
                Ok(Some(ModelUpdate::ThinkingDelta { text })) => {
                    if let Some(capture) = &mut self.capture {
                        capture.push(text);
                    }
                    return Ok(Some(ModelUpdate::Progress));
                }
                Ok(Some(ModelUpdate::Finished { items, .. })) => {
                    if let Some(capture) = &mut self.capture {
                        capture.finish(Some(items));
                    }
                }
                Ok(None) | Err(_) => {
                    if let Some(capture) = &mut self.capture {
                        capture.finish(None);
                    }
                }
                _ => {}
            }
            result
        })
    }
}
struct Capture {
    request_id: String,
    sender: watch::Sender<ThinkingLog>,
    secrets: Arc<Vec<String>>,
    raw: String,
    available: bool,
    withheld: bool,
    ended: bool,
    last_publish: Instant,
}
impl Capture {
    fn new(
        request_id: String,
        sender: watch::Sender<ThinkingLog>,
        secrets: Arc<Vec<String>>,
    ) -> Self {
        sender.send_modify(|log| {
            log.entries.retain(|entry| entry.request_id != request_id);
            log.entries.push(ThinkingSnapshot {
                request_id: request_id.clone(),
                text: String::new(),
                active: true,
                truncated: false,
                available: false,
            });
            bound_log(log);
        });
        Self {
            request_id,
            sender,
            secrets,
            raw: String::new(),
            available: false,
            withheld: false,
            ended: false,
            last_publish: Instant::now(),
        }
    }
    fn push(&mut self, text: &str) {
        if self.ended || text.is_empty() {
            return;
        }
        self.available = true;
        if !self.withheld {
            if self.raw.len().saturating_add(text.len()) > MAX_RAW_BYTES {
                self.raw = String::new();
                self.withheld = true;
            } else {
                self.raw.push_str(text);
            }
        }
        if self.last_publish.elapsed() >= PUBLISH_INTERVAL {
            self.publish(true);
        }
    }
    fn finish(&mut self, items: Option<&[HistoryItem]>) {
        if self.ended {
            return;
        }
        if let Some(items) = items {
            let native = reasoning(items);
            self.available |= native.available;
            if native.overflow {
                self.raw = String::new();
                self.withheld = true;
            } else if native.available && !self.withheld {
                self.raw = native.text;
            }
        }
        self.ended = true;
        self.publish(false);
    }
    fn publish(&mut self, active: bool) {
        let mut text = String::new();
        let mut truncated = self.withheld;
        // An incomplete secret can cross any delta or the display truncation
        // boundary. Keep the original complete buffer until terminal outcome.
        if !self.withheld && (!active || self.secrets.iter().all(String::is_empty)) {
            let mut value = Value::String(self.raw.clone());
            if saya_core::redaction::redact(&mut value, &self.secrets).is_ok() {
                if let Value::String(safe) = value {
                    let end = boundary(&safe, MAX_ENTRY_BYTES);
                    truncated |= end < safe.len();
                    text = safe[..end].to_owned();
                }
            } else {
                truncated = true;
            }
        }
        self.sender.send_modify(|log| {
            if let Some(entry) = log
                .entries
                .iter_mut()
                .find(|entry| entry.request_id == self.request_id)
            {
                entry.text = text;
                entry.active = active;
                entry.available = self.available;
                entry.truncated = truncated;
            }
            bound_log(log);
        });
        self.last_publish = Instant::now();
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.finish(None);
    }
}
fn boundary(text: &str, limit: usize) -> usize {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}
fn bound_log(log: &mut ThinkingLog) {
    let mut bytes = log
        .entries
        .iter()
        .fold(0usize, |sum, entry| sum.saturating_add(entry.text.len()));
    while log.entries.len() > MAX_ENTRIES || bytes > MAX_LOG_BYTES {
        if log.entries.is_empty() {
            break;
        }
        bytes = bytes.saturating_sub(log.entries.remove(0).text.len());
    }
}
#[derive(Default)]
struct Reasoning {
    text: String,
    available: bool,
    overflow: bool,
}
impl Reasoning {
    fn append(&mut self, text: &str) {
        if text.is_empty() || self.overflow {
            return;
        }
        self.available = true;
        let separator = usize::from(!self.text.is_empty());
        if self
            .text
            .len()
            .saturating_add(separator)
            .saturating_add(text.len())
            > MAX_RAW_BYTES
        {
            self.text = String::new();
            self.overflow = true;
            return;
        }
        if separator != 0 {
            self.text.push('\n');
        }
        self.text.push_str(text);
    }
}
fn reasoning(items: &[HistoryItem]) -> Reasoning {
    let mut result = Reasoning::default();
    for item in items {
        let HistoryItem::ProviderItem { protocol, value } = item else {
            continue;
        };
        match protocol {
            ModelProtocol::ChatCompletions => {
                if let Some(text) = value.get("reasoning_content").and_then(Value::as_str) {
                    result.append(text);
                }
            }
            ModelProtocol::AnthropicMessages if value["type"] == "thinking" => {
                if let Some(text) = value.get("thinking").and_then(Value::as_str) {
                    result.append(text);
                }
            }
            ModelProtocol::Responses if value["type"] == "reasoning" => {
                for (field, kind) in [("summary", "summary_text"), ("content", "reasoning_text")] {
                    if let Some(parts) = value.get(field).and_then(Value::as_array) {
                        for part in parts {
                            if part["type"] == kind
                                && let Some(text) = part.get("text").and_then(Value::as_str)
                            {
                                result.append(text);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    result
}
/// Read only documented plaintext fields from native provider history.
/// Opaque signatures, encrypted and redacted blocks are ignored. Aggregate
/// plaintext exceeding 16 MiB is withheld. Callers must redact the returned full
/// text with current known credentials before applying their display limit.
pub fn extract_reasoning(items: &[HistoryItem]) -> String {
    reasoning(items).text
}
