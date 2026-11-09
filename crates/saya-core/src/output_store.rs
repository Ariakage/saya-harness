//! Bounded, redacted output objects owned by the host.
//!
//! Capture buffers never write raw bytes to disk. Sealing saves one private,
//! content-addressed record. Publication is a separate host acknowledgement
//! after its tool result has committed to the session store. It is deliberately
//! not a transaction across the output directory and that session store.
use crate::{
    contract::{BoundaryError, ErrorInfo, RunId, SessionId},
    credentials::SecretRegistry,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

mod disk;
mod query;
#[cfg(test)]
mod tests;

const BOUNDARY_GUARD: usize = 64 * 1024;

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

/// Payload quotas count retained UTF-8 bytes. JSON metadata/escaping is bounded
/// separately by the record reader. A caller's older capture cap still applies.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutputLimits {
    pub max_object_bytes: usize,
    pub max_run_bytes: usize,
    pub max_store_bytes: usize,
    pub max_objects: usize,
    pub tail_bytes: usize,
    pub preview_bytes: usize,
    pub max_page_bytes: usize,
    pub max_page_lines: usize,
    pub max_search_matches: usize,
}
impl Default for OutputLimits {
    fn default() -> Self {
        Self {
            max_object_bytes: 16 * 1024 * 1024,
            max_run_bytes: 128 * 1024 * 1024,
            max_store_bytes: 1024 * 1024 * 1024,
            max_objects: 4096,
            tail_bytes: 4096,
            preview_bytes: 4096,
            max_page_bytes: 16 * 1024,
            max_page_lines: 200,
            max_search_matches: 200,
        }
    }
}
impl OutputLimits {
    pub fn validate(&self) -> Result<(), BoundaryError> {
        if self.max_object_bytes == 0
            || self.max_object_bytes > 64 * 1024 * 1024
            || self.max_run_bytes < self.max_object_bytes
            || self.max_store_bytes < self.max_run_bytes
            || self.max_objects == 0
            || self.max_objects > 16_384
            || self.tail_bytes > 64 * 1024
            || self.preview_bytes > 16 * 1024
            || self.max_page_bytes == 0
            || self.max_page_bytes > 64 * 1024
            || self.max_page_lines == 0
            || self.max_page_lines > 1000
            || self.max_search_matches == 0
            || self.max_search_matches > 1000
        {
            return Err(error("output_config", "Output limits are invalid"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    Stdout,
    Stderr,
    Mcp,
    Text,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputScope {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub call_id: String,
    pub stream: OutputStream,
}
impl OutputScope {
    fn validate(&self, secrets: &[String]) -> Result<(), BoundaryError> {
        if self.call_id.is_empty()
            || self.call_id.len() > 256
            || self.call_id.chars().any(char::is_control)
        {
            return Err(error("output_scope", "Output call identity is invalid"));
        }
        let serialized = serde_json::to_string(self)
            .map_err(|_| error("output_scope", "Output identity cannot be encoded"))?;
        if crate::redaction::contains_secret(&serialized, secrets) {
            return Err(error(
                "sensitive_data",
                "Output identity contains protected data",
            ));
        }
        Ok(())
    }
}

/// Whether the producer reached EOF. This records capture completion, never a
/// command exit code or the execution outcome of its parent operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureCompletion {
    Eof,
    Cancelled,
    ReadFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputRange {
    pub start: u64,
    pub end: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputCoverage {
    Complete,
    RetainedRangesOnly,
    Withheld,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WithheldReason {
    NonUtf8,
    EncodedBoundary,
    RedactionFailed,
    RedactionExpansion,
    CaptureDisabled,
}

/// Ranges refer to observed source bytes before redaction. Redaction can change
/// text lengths, so read/search cursors refer to the sealed UTF-8 segments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputMetadata {
    pub output_ref: String,
    pub scope: OutputScope,
    pub capture_limit_bytes: usize,
    pub total_bytes_seen: u64,
    pub retained_bytes: usize,
    pub retained_source_bytes: u64,
    pub retained_ranges: Vec<OutputRange>,
    pub gaps: Vec<OutputRange>,
    pub artifact_complete: bool,
    pub completion: CaptureCompletion,
    pub coverage: OutputCoverage,
    pub withheld_reason: Option<WithheldReason>,
    pub preview_head: String,
    pub preview_tail: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputCursor {
    pub segment: usize,
    pub byte_offset: usize,
    pub view_sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadOptions {
    pub cursor: Option<OutputCursor>,
    pub max_bytes: usize,
    pub max_lines: usize,
}
impl Default for ReadOptions {
    fn default() -> Self {
        Self {
            cursor: None,
            max_bytes: 8192,
            max_lines: 100,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchOptions {
    pub pattern: String,
    pub regex: bool,
    pub max_matches: usize,
    pub context_lines: usize,
    pub max_bytes: usize,
    pub cursor: Option<OutputCursor>,
}
impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            pattern: String::new(),
            regex: false,
            max_matches: 50,
            context_lines: 2,
            max_bytes: 8192,
            cursor: None,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputChunk {
    pub source_window: OutputRange,
    pub segment: usize,
    pub byte_offset: usize,
    pub text: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputPage {
    pub metadata: OutputMetadata,
    pub chunks: Vec<OutputChunk>,
    pub next_cursor: Option<OutputCursor>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchHit {
    pub source_window: OutputRange,
    pub segment: usize,
    pub line: usize,
    pub byte_offset: usize,
    pub text: String,
    pub truncated: bool,
    pub cursor: OutputCursor,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputSearchPage {
    pub metadata: OutputMetadata,
    pub matches: Vec<SearchHit>,
    pub next_cursor: Option<OutputCursor>,
}

#[derive(Clone)]
pub struct OutputStore {
    inner: Arc<disk::Inner>,
    run_pins: Option<Arc<RunPins>>,
}
struct RunPins {
    run_id: RunId,
    leases: std::sync::Mutex<std::collections::BTreeMap<String, OutputLease>>,
}
impl std::fmt::Debug for OutputStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OutputStore([PRIVATE])")
    }
}

/// A shared file lock keeps this exact object alive across processes and GC.
pub struct OutputLease {
    _file: std::fs::File,
}
impl std::fmt::Debug for OutputLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OutputLease([PRIVATE])")
    }
}

#[derive(Debug)]
pub struct SealedOutput {
    pub metadata: OutputMetadata,
    _lease: OutputLease,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputObjectState {
    Pending,
    Sealed,
    Published,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputInventory {
    pub reservation_id: String,
    pub scope: OutputScope,
    pub state: OutputObjectState,
    pub output_ref: Option<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GarbageCollection {
    pub removed: usize,
    pub leased: usize,
    pub pinned: usize,
    pub deferred: usize,
}

impl OutputStore {
    pub fn open(
        path: impl AsRef<Path>,
        limits: OutputLimits,
        secrets: Arc<SecretRegistry>,
    ) -> Result<Self, BoundaryError> {
        limits.validate()?;
        Ok(Self {
            inner: Arc::new(disk::Inner::open(path.as_ref(), limits, secrets)?),
            run_pins: None,
        })
    }
    /// Keep outputs published by this Run leased until its host handle drops.
    pub fn for_run(&self, run_id: RunId) -> Self {
        Self {
            inner: self.inner.clone(),
            run_pins: Some(Arc::new(RunPins {
                run_id,
                leases: std::sync::Mutex::new(Default::default()),
            })),
        }
    }
    /// Add this private root to the execution backend's protected paths.
    pub fn protected_path(&self) -> &Path {
        &self.inner.path
    }
    pub fn limits(&self) -> OutputLimits {
        self.inner.limits
    }

    /// Reserve quota before launching a producer. The effective limit is the
    /// minimum of this argument, the host object cap and remaining quotas.
    pub fn begin(
        &self,
        scope: OutputScope,
        effective_capture_bytes: usize,
    ) -> Result<OutputCapture, BoundaryError> {
        let (record, lease) = self.inner.reserve(scope, effective_capture_bytes)?;
        let tail_limit = self
            .inner
            .limits
            .tail_bytes
            .min(record.capture_limit_bytes / 2);
        Ok(OutputCapture {
            store: self.clone(),
            record,
            _reservation: lease,
            head: Vec::new(),
            tail: Vec::new(),
            tail_limit,
            observed: 0,
        })
    }
    /// The caller must first verify this reference against its durable tool
    /// result, including session/run/call/stream scope. Publication is idempotent.
    pub fn publish(&self, session_id: SessionId, output_ref: &str) -> Result<(), BoundaryError> {
        // Acquire before publication so concurrent GC cannot reclaim the object
        // between the durable host acknowledgement and Run pin registration.
        let (payload, _, lease) = self.inner.load(session_id, output_ref, false)?;
        self.inner.publish(session_id, output_ref)?;
        if let Some(pins) = &self.run_pins
            && payload.scope.run_id == pins.run_id
        {
            pins.leases
                .lock()
                .map_err(|_| error("output_io", "Output Run pin lock failed"))?
                .entry(output_ref.into())
                .or_insert(lease);
        }
        Ok(())
    }
    pub fn describe(
        &self,
        session_id: SessionId,
        output_ref: &str,
    ) -> Result<OutputMetadata, BoundaryError> {
        let (payload, digest, _lease) = self.inner.load(session_id, output_ref, true)?;
        Ok(payload.metadata(&digest, self.inner.limits.preview_bytes))
    }
    pub fn read(
        &self,
        session_id: SessionId,
        output_ref: &str,
        options: ReadOptions,
    ) -> Result<OutputPage, BoundaryError> {
        self.read_cancellable(session_id, output_ref, options, &AtomicBool::new(false))
    }
    pub fn read_cancellable(
        &self,
        session_id: SessionId,
        output_ref: &str,
        options: ReadOptions,
        cancel: &AtomicBool,
    ) -> Result<OutputPage, BoundaryError> {
        let (payload, digest, _lease) = self
            .inner
            .load_cancellable(session_id, output_ref, true, cancel)?;
        query::read(payload, &digest, options, self.inner.limits, cancel)
    }
    pub fn search(
        &self,
        session_id: SessionId,
        output_ref: &str,
        options: SearchOptions,
    ) -> Result<OutputSearchPage, BoundaryError> {
        self.search_cancellable(session_id, output_ref, options, &AtomicBool::new(false))
    }
    pub fn search_cancellable(
        &self,
        session_id: SessionId,
        output_ref: &str,
        options: SearchOptions,
        cancel: &AtomicBool,
    ) -> Result<OutputSearchPage, BoundaryError> {
        check_cancelled(cancel)?;
        if crate::redaction::contains_secret(&options.pattern, &self.inner.secrets.snapshot()?) {
            return Err(error(
                "sensitive_data",
                "Output search contains protected data",
            ));
        }
        let (payload, digest, _lease) = self
            .inner
            .load_cancellable(session_id, output_ref, true, cancel)?;
        query::search(payload, &digest, options, self.inner.limits, cancel)
    }
    pub fn lease(
        &self,
        session_id: SessionId,
        output_ref: &str,
    ) -> Result<OutputLease, BoundaryError> {
        let (_, _, lease) = self.inner.load(session_id, output_ref, true)?;
        Ok(lease)
    }
    /// Host recovery may inspect sealed objects before deciding which durable
    /// tool references are safe to publish. This never publishes by itself.
    pub fn inventory(&self) -> Result<Vec<OutputInventory>, BoundaryError> {
        self.inner.inventory()
    }
    /// Supply all live durable ContextView references in addition to runtime
    /// leases. Unreferenced objects are collected in reservation order.
    pub fn collect_garbage(
        &self,
        pinned_refs: &BTreeSet<String>,
        max_remove: usize,
    ) -> Result<GarbageCollection, BoundaryError> {
        self.inner.collect_garbage(pinned_refs, max_remove)
    }
}

pub struct OutputCapture {
    store: OutputStore,
    record: Reservation,
    _reservation: OutputLease,
    head: Vec<u8>,
    tail: Vec<u8>,
    tail_limit: usize,
    observed: u64,
}
impl std::fmt::Debug for OutputCapture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputCapture")
            .field("reservation_id", &self.record.id)
            .field("observed", &self.observed)
            .finish_non_exhaustive()
    }
}
impl Drop for OutputCapture {
    fn drop(&mut self) {
        self.head.fill(0);
        self.tail.fill(0);
    }
}
impl OutputCapture {
    pub fn reservation_id(&self) -> &str {
        &self.record.id
    }
    pub fn scope(&self) -> &OutputScope {
        &self.record.scope
    }
    pub fn total_bytes_seen(&self) -> u64 {
        self.observed
    }
    /// Pure bounded-memory capture. Producers must keep draining after errors
    /// and capture limits, preserving their own actual execution outcome.
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), BoundaryError> {
        self.observed = self
            .observed
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| error("output_limit", "Observed output byte count overflowed"))?;
        if self.record.capture_limit_bytes == 0 {
            return Ok(());
        }
        let head_limit = self
            .record
            .capture_limit_bytes
            .saturating_add(BOUNDARY_GUARD);
        let count = bytes.len().min(head_limit.saturating_sub(self.head.len()));
        self.head.extend_from_slice(&bytes[..count]);
        let tail_limit = self.tail_limit.saturating_add(BOUNDARY_GUARD);
        if bytes.len() >= tail_limit {
            self.tail.clear();
            self.tail
                .extend_from_slice(&bytes[bytes.len() - tail_limit..]);
        } else {
            let remove = self
                .tail
                .len()
                .saturating_add(bytes.len())
                .saturating_sub(tail_limit);
            self.tail.drain(..remove);
            self.tail.extend_from_slice(bytes);
        }
        Ok(())
    }
    pub fn seal(self, completion: CaptureCompletion) -> Result<SealedOutput, BoundaryError> {
        let secrets = self.store.inner.secrets.snapshot()?;
        self.record.scope.validate(&secrets)?;
        let payload = self.payload(completion, &secrets)?;
        self.store.inner.seal(&self.record, payload)
    }
    fn payload(
        &self,
        completion: CaptureCompletion,
        secrets: &[String],
    ) -> Result<Payload, BoundaryError> {
        let cap = self.record.capture_limit_bytes;
        let mut payload = Payload {
            version: 1,
            id: self.record.id.clone(),
            scope: self.record.scope.clone(),
            capture_limit_bytes: cap,
            total_bytes_seen: self.observed,
            completion,
            segments: Vec::new(),
            withheld_reason: None,
        };
        if cap == 0 && self.observed > 0 {
            payload.withheld_reason = Some(WithheldReason::CaptureDisabled);
            return Ok(payload);
        }
        let truncated = self.observed > cap as u64;
        let head_end = if truncated {
            cap.saturating_sub(self.tail_limit)
        } else {
            self.head.len().min(cap)
        };
        let windows = if truncated {
            vec![
                (&self.head[..], 0, 0, head_end as u64),
                (
                    &self.tail[..],
                    self.observed.saturating_sub(self.tail.len() as u64),
                    self.observed.saturating_sub(self.tail_limit as u64),
                    self.observed,
                ),
            ]
        } else {
            vec![(&self.head[..], 0, 0, self.observed)]
        };
        for (bytes, base, start, end) in windows {
            if start >= end {
                continue;
            }
            match capture_segment(bytes, base, start, end, truncated, secrets) {
                Ok(Some(segment)) => payload.segments.push(segment),
                Ok(None) => {}
                Err(reason) => {
                    payload.withheld_reason.get_or_insert(reason);
                }
            }
        }
        if payload
            .segments
            .iter()
            .map(|segment| segment.text.len())
            .sum::<usize>()
            > cap
        {
            payload.segments.clear();
            payload.withheld_reason = Some(WithheldReason::RedactionExpansion);
        }
        Ok(payload)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reservation {
    version: u8,
    id: String,
    scope: OutputScope,
    capture_limit_bytes: usize,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Segment {
    source_window: OutputRange,
    text: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    version: u8,
    id: String,
    scope: OutputScope,
    capture_limit_bytes: usize,
    total_bytes_seen: u64,
    completion: CaptureCompletion,
    segments: Vec<Segment>,
    withheld_reason: Option<WithheldReason>,
}
impl Payload {
    fn digest(&self) -> Result<String, BoundaryError> {
        self.digest_cancellable(&AtomicBool::new(false))
    }
    fn digest_cancellable(&self, cancel: &AtomicBool) -> Result<String, BoundaryError> {
        check_cancelled(cancel)?;
        let mut digest = Sha256::new();
        let encoded = serde_json::to_writer(
            HashWriter {
                digest: &mut digest,
                cancel,
            },
            self,
        );
        check_cancelled(cancel)?;
        encoded.map_err(|_| error("output_corrupt", "Output record cannot be encoded"))?;
        Ok(digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }
    fn metadata(&self, digest: &str, preview_bytes: usize) -> OutputMetadata {
        let ranges = self
            .segments
            .iter()
            .map(|segment| segment.source_window)
            .collect::<Vec<_>>();
        let mut gaps = Vec::new();
        let mut previous = 0;
        for range in &ranges {
            if previous < range.start {
                gaps.push(OutputRange {
                    start: previous,
                    end: range.start,
                });
            }
            previous = range.end;
        }
        if previous < self.total_bytes_seen {
            gaps.push(OutputRange {
                start: previous,
                end: self.total_bytes_seen,
            });
        }
        let complete = self.completion == CaptureCompletion::Eof
            && gaps.is_empty()
            && self.withheld_reason.is_none();
        let preview_head = self
            .segments
            .first()
            .map(|segment| prefix(&segment.text, preview_bytes / 2).to_owned())
            .unwrap_or_default();
        let preview_tail = self
            .segments
            .last()
            .map(|segment| suffix(&segment.text, preview_bytes / 2).to_owned())
            .unwrap_or_default();
        OutputMetadata {
            output_ref: format!("{}_{digest}", self.id),
            scope: self.scope.clone(),
            capture_limit_bytes: self.capture_limit_bytes,
            total_bytes_seen: self.total_bytes_seen,
            retained_bytes: self.segments.iter().map(|segment| segment.text.len()).sum(),
            retained_source_bytes: ranges.iter().map(|range| range.end - range.start).sum(),
            retained_ranges: ranges,
            gaps,
            artifact_complete: complete,
            completion: self.completion,
            coverage: if complete {
                OutputCoverage::Complete
            } else if self.segments.is_empty() {
                OutputCoverage::Withheld
            } else {
                OutputCoverage::RetainedRangesOnly
            },
            withheld_reason: self.withheld_reason,
            preview_head,
            preview_tail,
            sha256: digest.into(),
        }
    }
}

struct HashWriter<'a> {
    digest: &'a mut Sha256,
    cancel: &'a AtomicBool,
}
impl std::io::Write for HashWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        for chunk in bytes.chunks(64 * 1024) {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(std::io::Error::other("Output hashing cancelled"));
            }
            self.digest.update(chunk);
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn check_cancelled(cancel: &AtomicBool) -> Result<(), BoundaryError> {
    if cancel.load(Ordering::Relaxed) {
        Err(error("cancelled", "Output query was cancelled"))
    } else {
        Ok(())
    }
}

fn prefix(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
fn suffix(text: &str, limit: usize) -> &str {
    let mut start = text.len().saturating_sub(limit);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

fn capture_segment(
    bytes: &[u8],
    base: u64,
    requested_start: u64,
    requested_end: u64,
    truncated: bool,
    secrets: &[String],
) -> Result<Option<Segment>, WithheldReason> {
    let mut leading = 0;
    if base > 0 {
        while bytes.get(leading).is_some_and(|byte| byte & 0xc0 == 0x80) && leading < 3 {
            leading += 1;
        }
    }
    let candidate = &bytes[leading..];
    let text = match std::str::from_utf8(candidate) {
        Ok(text) => text,
        Err(error) if error.error_len().is_none() => {
            std::str::from_utf8(&candidate[..error.valid_up_to()])
                .map_err(|_| WithheldReason::NonUtf8)?
        }
        Err(_) => return Err(WithheldReason::NonUtf8),
    };
    let base = base + leading as u64;
    let mut start = requested_start.saturating_sub(base).min(text.len() as u64) as usize;
    let mut end = requested_end.saturating_sub(base).min(text.len() as u64) as usize;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    if truncated && !secrets.is_empty() && text.contains('\\') {
        // A bounded window can split arbitrarily nested JSON escaping. Avoid
        // storing fragments which cannot be proved safe using the whole value.
        return Err(WithheldReason::EncodedBoundary);
    }
    for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
        for (offset, _) in text.match_indices(secret) {
            let secret_end = offset + secret.len();
            if offset < start && secret_end > start {
                start = secret_end;
            }
            if offset < end && secret_end > end {
                end = offset;
            }
        }
    }
    if start >= end {
        return Ok(None);
    }
    let mut value = serde_json::Value::String(text[start..end].to_owned());
    crate::redaction::redact(&mut value, secrets).map_err(|_| WithheldReason::RedactionFailed)?;
    let serde_json::Value::String(redacted) = value else {
        return Err(WithheldReason::RedactionFailed);
    };
    Ok(Some(Segment {
        source_window: OutputRange {
            start: base + start as u64,
            end: base + end as u64,
        },
        text: redacted,
    }))
}

/// Validate the host-only registration against its durable producer result.
pub(crate) fn validate_references(
    outputs: &[OutputMetadata],
    session_id: SessionId,
    run_id: RunId,
    call_id: &str,
    result: &crate::contract::ToolResult,
) -> Result<(), BoundaryError> {
    if outputs.is_empty() || outputs.len() > 8 {
        return Err(error(
            "output_registration",
            "Output registration count is invalid",
        ));
    }
    let mut refs = BTreeSet::new();
    for metadata in outputs {
        metadata.scope.validate(&[])?;
        if metadata.scope.session_id != session_id
            || metadata.scope.run_id != run_id
            || metadata.scope.call_id != call_id
            || !refs.insert(&metadata.output_ref)
            || metadata.output_ref.len() > 160
            || !metadata
                .output_ref
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(error(
                "output_registration",
                "Output registration identity is invalid",
            ));
        }
    }
    let encoded = serde_json::to_value(outputs).map_err(|_| {
        error(
            "output_registration",
            "Output registration cannot be encoded",
        )
    })?;
    if encoded.to_string().len() > 128 * 1024
        || result
            .data
            .as_ref()
            .and_then(|value| value.get("output_refs"))
            != Some(&encoded)
    {
        return Err(error(
            "output_registration",
            "Output registration does not match its tool result",
        ));
    }
    Ok(())
}

pub(crate) fn validate_changes(
    changes: &[crate::boundary::StoreChange],
) -> Result<(), BoundaryError> {
    use crate::boundary::StoreChange;
    let mut calls = BTreeSet::new();
    for change in changes {
        if let StoreChange::OutputReferences {
            run_id,
            call_id,
            outputs,
        } = change
            && (!calls.insert((run_id.0, call_id))
                || outputs.is_empty()
                || !changes.iter().any(|change| {
                    matches!(change,
                    StoreChange::ToolFinished {run_id: producer, call_id: id, ..}
                    if producer == run_id && id == call_id)
                }))
        {
            return Err(error(
                "output_registration",
                "Output registration requires one atomic producer result",
            ));
        }
    }
    Ok(())
}

/// A host-only adapter return channel, separate from untrusted result data.
pub struct CapturedToolResult {
    pub result: crate::contract::ToolResult,
    pub outputs: Vec<SealedOutput>,
    pub capture_errors: Vec<ErrorInfo>,
}

pub(crate) struct CaptureWriter<'a>(pub &'a mut OutputCapture);
impl std::io::Write for CaptureWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .push(bytes)
            .map_err(|_| std::io::Error::other("Output capture failed"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
