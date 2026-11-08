//! Whitelisted tracing collection with bounded priority queues and exit drain.
use super::{DiagnosticRecord, JsonLog, LogContext, LogFields, LogLevel, failure};
use crate::contract::BoundaryError;
use std::{
    fmt::{self, Write as _},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tracing::{
    Event, Metadata, Subscriber,
    field::{Field, Visit},
    span::{Attributes, Id, Record},
};
use tracing_subscriber::{Layer, layer::Context, prelude::*, registry::LookupSpan};

const NORMAL_CAPACITY: usize = 256;
const PRIORITY_CAPACITY: usize = 32;
const ENQUEUE_WAIT: Duration = Duration::from_millis(20);
const EXIT_WAIT: Duration = Duration::from_secs(2);

enum Item {
    Record(Box<DiagnosticRecord>),
    Flush(mpsc::Sender<Result<(), BoundaryError>>),
}
struct State {
    closed: AtomicBool,
    dropped: AtomicU64,
    failed: AtomicBool,
}
#[derive(Clone)]
struct Queue {
    normal: SyncSender<Item>,
    priority: SyncSender<Item>,
    state: Arc<State>,
    minimum: LogLevel,
}
impl Queue {
    fn record(&self, record: DiagnosticRecord) {
        if self.state.closed.load(Ordering::Acquire) || record.level < self.minimum {
            return;
        }
        let priority = record.level >= LogLevel::Warn;
        let sender = if priority {
            &self.priority
        } else {
            &self.normal
        };
        let result = if priority {
            send_until(
                sender,
                Item::Record(Box::new(record)),
                Instant::now() + ENQUEUE_WAIT,
            )
        } else {
            sender
                .try_send(Item::Record(Box::new(record)))
                .map_err(|_| ())
        };
        if result.is_err() {
            self.state.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}
fn send_until(sender: &SyncSender<Item>, mut item: Item, deadline: Instant) -> Result<(), ()> {
    loop {
        match sender.try_send(item) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Full(returned)) if Instant::now() < deadline => {
                item = returned;
                thread::sleep(Duration::from_millis(1));
            }
            Err(_) => return Err(()),
        }
    }
}

/// Owns the local diagnostic writer. Keep it alive until all application tasks end.
/// Queue saturation increments a loss counter. Flush and drop have bounded waits.
pub struct Diagnostics {
    queue: Queue,
    done: Receiver<()>,
    worker: Option<JoinHandle<()>>,
}
impl Diagnostics {
    pub fn start(log: JsonLog) -> Result<Self, BoundaryError> {
        Self::with_capacity(log, NORMAL_CAPACITY, PRIORITY_CAPACITY)
    }
    fn with_capacity(log: JsonLog, normal: usize, priority: usize) -> Result<Self, BoundaryError> {
        let (normal_tx, normal_rx) = mpsc::sync_channel(normal);
        let (priority_tx, priority_rx) = mpsc::sync_channel(priority);
        let (done_tx, done) = mpsc::channel();
        let state = Arc::new(State {
            closed: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
            failed: AtomicBool::new(false),
        });
        let queue = Queue {
            normal: normal_tx,
            priority: priority_tx,
            state: state.clone(),
            minimum: log.level,
        };
        let worker = thread::Builder::new()
            .name("saya-diagnostics".into())
            .spawn(move || {
                write_loop(log, normal_rx, priority_rx, state);
                let _ = done_tx.send(());
            })
            .map_err(|_| failure())?;
        Ok(Self {
            queue,
            done,
            worker: Some(worker),
        })
    }
    /// Install this dispatch in the executable or a scoped test. Only saya targets
    /// and the typed field whitelist are collected. No formatting layer writes stdout.
    pub fn dispatch(&self) -> tracing::Dispatch {
        tracing::Dispatch::new(tracing_subscriber::registry().with(SafeLayer {
            queue: self.queue.clone(),
        }))
    }
    /// Number of events lost to a full queue or unavailable worker.
    pub fn dropped(&self) -> u64 {
        self.queue.state.dropped.load(Ordering::Relaxed)
    }
    /// Wait for previously queued records, including the priority lane, to reach
    /// the local writer. Disk failures and deadline expiry return log_error.
    pub fn flush(&self, timeout: Duration) -> Result<(), BoundaryError> {
        let deadline = Instant::now().checked_add(timeout).ok_or_else(failure)?;
        let (tx, rx) = mpsc::channel();
        send_until(&self.queue.normal, Item::Flush(tx), deadline).map_err(|_| failure())?;
        rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| failure())?
    }
}
impl Drop for Diagnostics {
    fn drop(&mut self) {
        self.queue.state.closed.store(true, Ordering::Release);
        if self.done.recv_timeout(EXIT_WAIT).is_ok()
            && let Some(worker) = self.worker.take()
        {
            let _ = worker.join();
        }
    }
}
fn write_record(log: &JsonLog, record: &DiagnosticRecord, state: &State) {
    if log.record_typed(record).is_err() {
        state.failed.store(true, Ordering::Release);
    }
}
fn report_dropped(log: &JsonLog, state: &State, reported: &mut u64) {
    let total = state.dropped.load(Ordering::Relaxed);
    if total > *reported {
        let mut record = DiagnosticRecord::new(LogLevel::Warn, "diagnostics.dropped");
        record.fields.dropped = Some(total - *reported);
        write_record(log, &record, state);
        *reported = total;
    }
}
fn write_loop(log: JsonLog, normal: Receiver<Item>, priority: Receiver<Item>, state: Arc<State>) {
    let mut reported = 0;
    loop {
        while let Ok(Item::Record(record)) = priority.try_recv() {
            write_record(&log, &record, &state);
        }
        match normal.recv_timeout(Duration::from_millis(5)) {
            Ok(Item::Record(record)) => write_record(&log, &record, &state),
            Ok(Item::Flush(reply)) => {
                // Priority records submitted before the barrier must be saved too.
                while let Ok(Item::Record(record)) = priority.try_recv() {
                    write_record(&log, &record, &state);
                }
                report_dropped(&log, &state, &mut reported);
                let result = if state.failed.load(Ordering::Acquire) {
                    Err(failure())
                } else {
                    Ok(())
                };
                let _ = reply.send(result);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                report_dropped(&log, &state, &mut reported);
                if state.closed.load(Ordering::Acquire) {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

#[derive(Clone, Default)]
struct Captured {
    event: Option<String>,
    context: LogContext,
    fields: LogFields,
}
fn bounded(value: &str) -> String {
    if value.len() > 1024 {
        "[WITHHELD]".into()
    } else {
        value.into()
    }
}
impl Captured {
    fn string(&mut self, name: &str, value: &str) {
        let slot = match name {
            "event" => &mut self.event,
            "session_id" => &mut self.context.session_id,
            "run_id" => &mut self.context.run_id,
            "request_id" => &mut self.context.request_id,
            "call_id" => &mut self.context.call_id,
            "job_id" => &mut self.context.job_id,
            "status" => &mut self.fields.status,
            "error_code" => &mut self.fields.error_code,
            "profile" => &mut self.fields.profile,
            "protocol" => &mut self.fields.protocol,
            "purpose" => &mut self.fields.purpose,
            _ => return,
        };
        *slot = Some(bounded(value));
    }
    fn inherit(&mut self, parent: &Self) {
        macro_rules! copy {
            ($group:ident, $($field:ident),+) => {$(if parent.$group.$field.is_some() { self.$group.$field.clone_from(&parent.$group.$field); })+};
        }
        copy!(
            context, session_id, run_id, request_id, call_id, job_id, span_id
        );
        copy!(fields, profile, protocol, purpose);
    }
}
struct BoundedDebug(String);
impl fmt::Write for BoundedDebug {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let available = 1024_usize.saturating_sub(self.0.len());
        let mut end = value.len().min(available);
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        self.0.push_str(&value[..end]);
        if end < value.len() {
            Err(fmt::Error)
        } else {
            Ok(())
        }
    }
}
impl Visit for Captured {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.string(field.name(), value);
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        let slot = match field.name() {
            "duration_ms" => &mut self.fields.duration_ms,
            "bytes" => &mut self.fields.bytes,
            "count" => &mut self.fields.count,
            "dropped" => &mut self.fields.dropped,
            _ => return,
        };
        *slot = Some(value);
    }
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if !matches!(
            field.name(),
            "event"
                | "session_id"
                | "run_id"
                | "request_id"
                | "call_id"
                | "job_id"
                | "status"
                | "error_code"
                | "profile"
                | "protocol"
                | "purpose"
        ) {
            return;
        }
        let mut buffer = BoundedDebug(String::new());
        if write!(&mut buffer, "{value:?}").is_err() {
            self.string(field.name(), "[WITHHELD]");
        } else {
            self.string(field.name(), &buffer.0);
        }
    }
}
struct SafeLayer {
    queue: Queue,
}
fn level(level: &tracing::Level) -> LogLevel {
    match *level {
        tracing::Level::TRACE => LogLevel::Trace,
        tracing::Level::DEBUG => LogLevel::Debug,
        tracing::Level::INFO => LogLevel::Info,
        tracing::Level::WARN => LogLevel::Warn,
        tracing::Level::ERROR => LogLevel::Error,
    }
}
impl<S> Layer<S> for SafeLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn max_level_hint(&self) -> Option<tracing::metadata::LevelFilter> {
        Some(tracing::metadata::LevelFilter::TRACE)
    }
    fn enabled(&self, metadata: &Metadata<'_>, _ctx: Context<'_, S>) -> bool {
        (metadata.target() == "saya" || metadata.target().starts_with("saya::"))
            && (metadata.is_span() || level(metadata.level()) >= self.queue.minimum)
    }
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            let mut captured = Captured::default();
            attrs.record(&mut captured);
            captured.context.span_id = Some(id.clone().into_u64().to_string());
            span.extensions_mut().insert(captured);
        }
    }
    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id)
            && let Some(captured) = span.extensions_mut().get_mut::<Captured>()
        {
            values.record(captured);
        }
    }
    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut captured = Captured::default();
        if let Some(scope) = ctx.event_scope(event) {
            for span in scope.from_root() {
                if let Some(parent) = span.extensions().get::<Captured>() {
                    captured.inherit(parent);
                }
            }
        }
        event.record(&mut captured);
        let record = DiagnosticRecord {
            level: level(event.metadata().level()),
            event: captured.event.unwrap_or_else(|| "diagnostic.event".into()),
            context: captured.context,
            fields: captured.fields,
        };
        self.queue.record(record);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::LogEntry;
    #[tokio::test]
    async fn concurrent_spans_keep_explicit_task_ancestry_and_hide_payloads() {
        use tracing::{Instrument, instrument::WithSubscriber};
        let dir = tempfile::tempdir().unwrap();
        let diagnostics = Diagnostics::start(
            JsonLog::new(dir.path(), vec!["known-fixture".into()]).with_level(LogLevel::Trace),
        )
        .unwrap();
        let dispatch = diagnostics.dispatch();
        let mut tasks = Vec::new();
        for number in 0..4 {
            let span = tracing::dispatcher::with_default(
                &dispatch,
                || tracing::info_span!(target: "saya::test", "run", session_id = "session", run_id = number.to_string()),
            );
            tasks.push(tokio::spawn(async move {
                tokio::task::yield_now().await;
                tracing::info!(target: "saya::test", event = "model.finished", request_id = "request", purpose = "execution", status = "known-fixture", prompt = "private prompt", message = "private body");
                tracing::error!(target: "http_client", event = "foreign.event", status = "private foreign body");
            }.instrument(span).with_subscriber(dispatch.clone())));
        }
        for task in tasks {
            task.await.unwrap();
        }
        diagnostics.flush(Duration::from_secs(2)).unwrap();
        let text = std::fs::read_to_string(dir.path().join("saya.log")).unwrap();
        assert!(!text.contains("private"));
        assert!(!text.contains("known-fixture"));
        let records: Vec<LogEntry> = text
            .lines()
            .map(|line| LogEntry::decode(line.as_bytes(), &[]).unwrap())
            .collect();
        assert_eq!(records.len(), 4);
        for record in records {
            let context = record.context.unwrap();
            assert_eq!(context.session_id.as_deref(), Some("session"));
            assert!(context.run_id.is_some());
            assert_eq!(context.request_id.as_deref(), Some("request"));
        }
    }
    #[test]
    fn overloaded_queues_report_loss_and_flush_errors_are_bounded() {
        let dir = tempfile::tempdir().unwrap();
        // Hold the writer lock so that the normal queue deterministically fills.
        let log = JsonLog::new(dir.path(), vec![]).with_level(LogLevel::Trace);
        log.record("warn", "initial", serde_json::json!({}))
            .unwrap();
        let lock = std::fs::File::open(dir.path().join("saya-log.lock")).unwrap();
        unsafe {
            libc::flock(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::LOCK_EX);
        }
        let diagnostics = Diagnostics::with_capacity(log, 1, 1).unwrap();
        for _ in 0..100 {
            diagnostics
                .queue
                .record(DiagnosticRecord::new(LogLevel::Debug, "busy"));
        }
        diagnostics
            .queue
            .record(DiagnosticRecord::new(LogLevel::Error, "priority"));
        assert!(diagnostics.dropped() > 0);
        assert!(diagnostics.flush(Duration::from_millis(10)).is_err());
        unsafe {
            libc::flock(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::LOCK_UN);
        }
        diagnostics.flush(Duration::from_secs(2)).unwrap();
        let text = std::fs::read_to_string(dir.path().join("saya.log")).unwrap();
        assert!(text.contains("priority"));
        assert!(text.contains("diagnostics.dropped"));
    }
    #[test]
    fn disk_failure_reaches_flush_without_exposing_details() {
        let dir = tempfile::tempdir().unwrap();
        let target = tempfile::NamedTempFile::new().unwrap();
        std::os::unix::fs::symlink(target.path(), dir.path().join("saya.log")).unwrap();
        let diagnostics = Diagnostics::start(JsonLog::new(dir.path(), vec![])).unwrap();
        diagnostics
            .queue
            .record(DiagnosticRecord::new(LogLevel::Error, "failure"));
        assert!(diagnostics.flush(Duration::from_secs(2)).is_err());
        assert!(std::fs::read(target.path()).unwrap().is_empty());
    }

    #[test]
    fn drop_closes_producers_before_waiting_for_full_queues_to_drain() {
        let dir = tempfile::tempdir().unwrap();
        let log = JsonLog::new(dir.path(), vec![]).with_level(LogLevel::Trace);
        log.record("warn", "initial", serde_json::json!({}))
            .unwrap();
        let lock = std::fs::File::open(dir.path().join("saya-log.lock")).unwrap();
        // SAFETY: the temporary lock descriptor stays alive through the unlock.
        assert_eq!(
            unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::LOCK_EX) },
            0
        );
        let diagnostics = Diagnostics::with_capacity(log, 1, 1).unwrap();
        for _ in 0..100 {
            diagnostics
                .queue
                .record(DiagnosticRecord::new(LogLevel::Debug, "normal"));
        }
        diagnostics
            .queue
            .record(DiagnosticRecord::new(LogLevel::Error, "priority-at-exit"));
        let state = diagnostics.queue.state.clone();
        let unlock = std::thread::spawn(move || {
            let deadline = Instant::now() + EXIT_WAIT + Duration::from_secs(1);
            while !state.closed.load(Ordering::Acquire) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(state.closed.load(Ordering::Acquire));
            // SAFETY: this thread owns the live lock descriptor.
            assert_eq!(
                unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::LOCK_UN) },
                0
            );
        });
        let started = Instant::now();
        drop(diagnostics);
        let elapsed = started.elapsed();
        unlock.join().unwrap();
        assert!(
            elapsed < EXIT_WAIT - Duration::from_millis(500),
            "exit drain took {elapsed:?}"
        );
        let text = std::fs::read_to_string(dir.path().join("saya.log")).unwrap();
        assert!(text.contains("priority-at-exit"));
        assert!(text.contains("diagnostics.dropped"));
    }
}
