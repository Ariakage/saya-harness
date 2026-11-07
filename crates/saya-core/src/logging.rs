//! Local structured diagnostics with bounded retention and credential redaction.
use crate::{
    contract::{BoundaryError, ErrorInfo},
    redaction,
};
use cap_std::{
    ambient_authority,
    fs::{Dir, MetadataExt as _},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{DirBuilderExt, MetadataExt},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_BYTES: u64 = 10 * 1024 * 1024;
const BACKUPS: usize = 5;
const MAX_RECORD_BYTES: usize = 64 * 1024;

/// Severity threshold. A threshold includes events of equal or greater severity.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    #[default]
    Warn,
    Error,
}
impl std::str::FromStr for LogLevel {
    type Err = BoundaryError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "trace" => Ok(Self::Trace),
            "debug" => Ok(Self::Debug),
            "info" => Ok(Self::Info),
            "warn" => Ok(Self::Warn),
            "error" => Ok(Self::Error),
            _ => Err(failure()),
        }
    }
}

/// Allowed diagnostic correlation identifiers. Payloads and credentials have no field here.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,
}

/// Whitelisted lifecycle metadata accepted by the structured diagnostics API.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dropped: Option<u64>,
}

/// One diagnostic event. Unknown application data is deliberately absent.
#[derive(Clone, Debug)]
pub struct DiagnosticRecord {
    pub level: LogLevel,
    pub event: String,
    pub context: LogContext,
    pub fields: LogFields,
}
impl DiagnosticRecord {
    pub fn new(level: LogLevel, event: impl Into<String>) -> Self {
        Self {
            level,
            event: event.into(),
            context: LogContext::default(),
            fields: LogFields::default(),
        }
    }
}

/// Decoded local record. Missing metadata remains absent for legacy log rows.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u32>,
    pub timestamp: String,
    pub level: LogLevel,
    pub event: String,
    #[serde(default)]
    pub data: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<LogContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_id: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
}
impl LogEntry {
    /// Decode a bounded legacy or current row and reapply current redaction.
    pub fn decode(line: &[u8], secrets: &[String]) -> Result<Self, BoundaryError> {
        if line.len() > MAX_RECORD_BYTES {
            return Err(failure());
        }
        let mut value: Value = serde_json::from_slice(line).map_err(|_| failure())?;
        redaction::redact(&mut value, secrets).map_err(|_| failure())?;
        let entry: Self = serde_json::from_value(value).map_err(|_| failure())?;
        if entry.schema_version.is_some_and(|version| version != 1) {
            return Err(failure());
        }
        Ok(entry)
    }
}

fn failure() -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: "log_error".into(),
            message: "cannot write structured log".into(),
        },
        None,
        None,
    )
}

/// Logger for one configured state directory. It does not send data elsewhere.
#[derive(Clone)]
pub struct JsonLog {
    root: PathBuf,
    secrets: Vec<String>,
    max_bytes: u64,
    backups: usize,
    level: LogLevel,
    sequence: Arc<AtomicU64>,
}
impl JsonLog {
    pub fn new(root: impl Into<PathBuf>, secrets: Vec<String>) -> Self {
        Self {
            root: root.into(),
            secrets,
            max_bytes: MAX_BYTES,
            backups: BACKUPS,
            level: LogLevel::Warn,
            sequence: Arc::new(AtomicU64::new(1)),
        }
    }
    /// Select the minimum severity without opening or creating log files.
    pub fn with_level(mut self, level: LogLevel) -> Self {
        self.level = level;
        self
    }
    pub fn enabled(&self, level: LogLevel) -> bool {
        level >= self.level
    }
    /// Append a typed event with schema version, process and local sequence.
    pub fn record_typed(&self, event: &DiagnosticRecord) -> Result<(), BoundaryError> {
        if !self.enabled(event.level) {
            return Ok(());
        }
        if event.event.is_empty()
            || event.event.len() > 128
            || !event
                .event
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || b"_.-".contains(&ch))
        {
            return Err(failure());
        }
        let mut record = json!({
            "schema_version": 1, "timestamp": timestamp()?, "level": event.level,
            "event": event.event, "target": "saya", "context": event.context,
            "data": event.fields, "process_id": std::process::id(),
            "sequence": self.sequence.fetch_add(1, Ordering::Relaxed)
        });
        self.append(&mut record)
    }
    /// Compatibility entry point for existing warning/error records. New callers
    /// should use the typed, whitelisted API. Every accepted level is filtered.
    pub fn record(&self, level: &str, event: &str, data: Value) -> Result<(), BoundaryError> {
        let level: LogLevel = level.parse()?;
        if !self.enabled(level) {
            return Ok(());
        }
        let mut record = json!({"timestamp":timestamp()?,"level":level,"event":event,"data":data});
        self.append(&mut record)
    }
    fn append(&self, record: &mut Value) -> Result<(), BoundaryError> {
        redaction::redact(record, &self.secrets).map_err(|_| failure())?;
        let mut line = serde_json::to_vec(record).map_err(|_| failure())?;
        line.push(b'\n');
        if line.len() > MAX_RECORD_BYTES
            || u64::try_from(line.len()).map_err(|_| failure())? > self.max_bytes
        {
            return Err(failure());
        }
        let dir = open_root(&self.root).map_err(|_| failure())?;
        let lock = open_private(&dir, "saya-log.lock").map_err(|_| failure())?;
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            // SAFETY: lock is a live file descriptor and flock retains no pointer.
            if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                break;
            }
            if !matches!(
                io::Error::last_os_error().kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            ) || Instant::now() >= deadline
            {
                return Err(failure());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut file = open_private(&dir, "saya.log").map_err(|_| failure())?;
        if file
            .metadata()
            .map_err(|_| failure())?
            .len()
            .saturating_add(line.len() as u64)
            > self.max_bytes
        {
            drop(file);
            for i in (1..=self.backups).rev() {
                let dest = format!("saya.log.{i}");
                validate_existing(&dir, &dest).map_err(|_| failure())?;
                if i == self.backups {
                    remove_if_exists(&dir, &dest).map_err(|_| failure())?;
                }
                let source = if i == 1 {
                    "saya.log".to_owned()
                } else {
                    format!("saya.log.{}", i - 1)
                };
                validate_existing(&dir, &source).map_err(|_| failure())?;
                match dir.rename(&source, &dir, &dest) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(_) => return Err(failure()),
                }
            }
            file = open_private(&dir, "saya.log").map_err(|_| failure())?;
        }
        file.write_all(&line).map_err(|_| failure())?;
        file.flush().map_err(|_| failure())?;
        Ok(())
    }
}
fn open_root(root: &Path) -> io::Result<Dir> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(root)?;
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o022 != 0
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Dir::open_ambient_dir(root, ambient_authority())
}
fn validate_existing(dir: &Dir, name: &str) -> io::Result<()> {
    match dir.symlink_metadata(name) {
        Ok(meta) if meta.file_type().is_file() && meta.nlink() == 1 && meta.mode() & 0o077 == 0 => {
            Ok(())
        }
        Ok(_) => Err(io::ErrorKind::InvalidInput.into()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}
fn remove_if_exists(dir: &Dir, name: &str) -> io::Result<()> {
    match dir.remove_file(name) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}
fn open_private(dir: &Dir, name: &str) -> io::Result<fs::File> {
    let name = std::ffi::CString::new(name).map_err(|_| io::ErrorKind::InvalidInput)?;
    let mut attempts = 0;
    let fd = loop {
        // SAFETY: dir is live, name is NUL terminated, and the returned descriptor
        // is transferred to File exactly once.
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY
                    | libc::O_APPEND
                    | libc::O_CREAT
                    | libc::O_NOFOLLOW
                    | libc::O_CLOEXEC
                    | libc::O_NONBLOCK,
                0o600,
            )
        };
        if fd >= 0 {
            break fd;
        }
        let error = io::Error::last_os_error();
        attempts += 1;
        if attempts >= 100
            || !matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::Interrupted | io::ErrorKind::AlreadyExists
            )
        {
            return Err(error);
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    };
    let file = unsafe { fs::File::from_raw_fd(fd) };
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(file)
}
fn timestamp() -> Result<String, BoundaryError> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| failure())?
        .as_secs();
    let seconds = libc::time_t::try_from(seconds).map_err(|_| failure())?;
    let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: gmtime_r writes the tm on success and retains neither pointer.
    if unsafe { libc::gmtime_r(&seconds, tm.as_mut_ptr()) }.is_null() {
        return Err(failure());
    }
    let tm = unsafe { tm.assume_init() };
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn levels_filter_before_io_and_accept_all_five_severities() {
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("unused");
        let default_log = JsonLog::new(&absent, vec![]);
        default_log
            .record_typed(&DiagnosticRecord::new(LogLevel::Info, "run.started"))
            .unwrap();
        assert!(!absent.exists());
        let log = default_log.with_level(LogLevel::Trace);
        for level in [
            LogLevel::Trace,
            LogLevel::Debug,
            LogLevel::Info,
            LogLevel::Warn,
            LogLevel::Error,
        ] {
            log.record_typed(&DiagnosticRecord::new(level, "run.phase"))
                .unwrap();
        }
        let rows = fs::read_to_string(absent.join("saya.log")).unwrap();
        let entries: Vec<_> = rows
            .lines()
            .map(|line| LogEntry::decode(line.as_bytes(), &[]).unwrap())
            .collect();
        assert_eq!(entries.len(), 5);
        assert!(entries.iter().all(|row| row.schema_version == Some(1)));
        assert_eq!(entries[4].sequence, Some(5));
        assert!(log.record("verbose", "invalid", json!({})).is_err());
    }

    #[test]
    fn typed_fields_reject_payloads_and_redact_nested_metadata() {
        assert!(serde_json::from_value::<LogFields>(json!({"prompt":"payload"})).is_err());
        assert!(serde_json::from_value::<LogContext>(json!({"authorization":"payload"})).is_err());
        let dir = tempfile::tempdir().unwrap();
        let secret = "fixture\"secret\\tail".to_string();
        let log = JsonLog::new(dir.path(), vec![secret.clone()]);
        let mut event = DiagnosticRecord::new(LogLevel::Warn, "request.failed");
        event.context.request_id = Some("request-1".into());
        event.fields.status =
            Some(json!({"nested":json!({"secret":secret}).to_string()}).to_string());
        log.record_typed(&event).unwrap();
        let row = fs::read(dir.path().join("saya.log")).unwrap();
        assert!(!redaction::contains_secret(
            std::str::from_utf8(&row).unwrap(),
            &[secret]
        ));
        assert!(std::str::from_utf8(&row).unwrap().contains("[REDACTED]"));
        event.event = "invalid\nname".into();
        assert!(log.record_typed(&event).is_err());
        event.event = "bounded".into();
        event.fields.status = Some("x".repeat(MAX_RECORD_BYTES));
        assert!(log.record_typed(&event).is_err());
    }

    #[test]
    fn reads_legacy_records_and_reapplies_current_redaction() {
        let old = br#"{"timestamp":"2026-11-08T00:00:00Z","level":"warn","event":"legacy","data":{"detail":"fixture-token"}}"#;
        let entry = LogEntry::decode(old, &["fixture-token".into()]).unwrap();
        assert!(entry.schema_version.is_none());
        assert_eq!(entry.data["detail"], "[REDACTED]");
        assert!(LogEntry::decode(b"partial", &[]).is_err());
    }

    #[test]
    fn rotation_and_known_secret_redaction_are_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = JsonLog::new(dir.path(), vec!["fake-secret".into()]);
        log.max_bytes = 180;
        log.backups = 2;
        for _ in 0..8 {
            log.record("warn", "test", json!({"detail":"fake-secret"}))
                .unwrap();
        }
        for entry in fs::read_dir(dir.path()).unwrap() {
            let path = entry.unwrap().path();
            if path
                .extension()
                .is_some_and(|extension| extension == "lock")
            {
                continue;
            }
            let contents = fs::read_to_string(path).unwrap();
            assert!(contents.len() <= 180);
            assert!(!contents.contains("fake-secret"));
            for line in contents.lines() {
                let value: Value = serde_json::from_str(line).unwrap();
                assert_eq!(value["data"]["detail"], "[REDACTED]");
            }
        }
        assert!(dir.path().join("saya.log.2").exists());
        assert!(!dir.path().join("saya.log.3").exists());
    }
    #[test]
    fn refuses_symlink_log_file() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        symlink(outside.path(), dir.path().join("saya.log")).unwrap();
        let log = JsonLog::new(dir.path(), vec![]);
        assert!(log.record("error", "test", json!({})).is_err());
        assert!(fs::read(outside.path()).unwrap().is_empty());
    }
    #[test]
    fn concurrent_writers_keep_complete_json_records() {
        let dir = tempfile::tempdir().unwrap();
        let log = JsonLog::new(dir.path(), vec![]);
        let workers: Vec<_> = (0..4)
            .map(|worker| {
                let log = log.clone();
                std::thread::spawn(move || {
                    for item in 0..25 {
                        log.record("warn", "concurrent", json!({"worker":worker,"item":item}))
                            .unwrap();
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let contents = fs::read_to_string(dir.path().join("saya.log")).unwrap();
        let records: Vec<Value> = contents
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records.len(), 100);
        assert!(records.iter().all(|record| record["event"] == "concurrent"));
    }
    #[test]
    fn refuses_wide_permissions_and_hard_linked_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("saya.log");
        fs::write(&path, b"original").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let log = JsonLog::new(dir.path(), vec![]);
        assert!(log.record("warn", "unsafe", json!({})).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::hard_link(&path, dir.path().join("linked")).unwrap();
        assert!(log.record("warn", "unsafe", json!({})).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"original");
    }
    #[test]
    fn logging_child() {
        let Some(root) = std::env::var_os("SAYA_TEST_LOG_ROOT") else {
            return;
        };
        let log = JsonLog::new(root, vec![]);
        for item in 0..25 {
            log.record("warn", "child", json!({"item":item})).unwrap();
        }
    }
    #[test]
    fn separate_processes_serialize_log_writes() {
        let dir = tempfile::tempdir().unwrap();
        let executable = std::env::current_exe().unwrap();
        let mut children: Vec<_> = (0..3)
            .map(|_| {
                std::process::Command::new(&executable)
                    .args(["--exact", "logging::tests::logging_child"])
                    .env("SAYA_TEST_LOG_ROOT", dir.path())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect();
        for child in &mut children {
            let status = child.wait().unwrap();
            assert!(status.success());
        }
        let contents = fs::read_to_string(dir.path().join("saya.log")).unwrap();
        let records: Vec<Value> = contents
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records.len(), 75);
    }
}
