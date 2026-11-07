//! Bounded read-only queries over current and rotated local diagnostics.
use super::{BACKUPS, LogEntry, LogLevel, MAX_BYTES, MAX_RECORD_BYTES, failure};
use crate::{boundary::Cancellation, contract::BoundaryError};
use cap_std::{
    ambient_authority,
    fs::{Dir, MetadataExt as _},
};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs,
    io::{self, BufRead, BufReader, Read},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    path::PathBuf,
};

/// Query limits apply after filters. Timestamps require an explicit UTC offset.
#[derive(Clone, Debug)]
pub struct LogQuery {
    pub minimum: LogLevel,
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub offset: usize,
    pub limit: usize,
}
impl Default for LogQuery {
    fn default() -> Self {
        Self {
            minimum: LogLevel::Trace,
            session_id: None,
            run_id: None,
            since: None,
            until: None,
            offset: 0,
            limit: 100,
        }
    }
}
#[derive(Debug, Serialize)]
pub struct LogPage {
    pub records: Vec<LogEntry>,
    pub next_offset: Option<usize>,
    pub malformed_lines: u64,
}
/// A live follower tracks file identity and complete-line offsets across rotation.
/// Tracking is limited to the six currently retained files.
#[derive(Default)]
pub struct LogCursor(BTreeMap<(u64, u64), FollowPosition>);
struct FollowPosition {
    offset: u64,
    // Holding the descriptor prevents inode reuse until the next complete view.
    _file: fs::File,
}

pub struct LogReader {
    root: PathBuf,
    secrets: Vec<String>,
}
impl LogReader {
    pub fn new(root: impl Into<PathBuf>, secrets: Vec<String>) -> Self {
        Self {
            root: root.into(),
            secrets,
        }
    }
    /// Read one bounded page without creating directories or taking a writer lock.
    pub fn page(
        &self,
        query: &LogQuery,
        cancellation: Option<&dyn Cancellation>,
    ) -> Result<LogPage, BoundaryError> {
        self.scan(query, None, cancellation)
    }
    /// Return newly completed records. Renames keep the inode cursor, while a new
    /// current file starts at zero. Partial trailing rows remain pending.
    pub fn follow(
        &self,
        query: &LogQuery,
        cursor: &mut LogCursor,
        cancellation: Option<&dyn Cancellation>,
    ) -> Result<LogPage, BoundaryError> {
        if query.offset != 0 {
            return Err(failure());
        }
        self.scan(query, Some(cursor), cancellation)
    }
    fn scan(
        &self,
        query: &LogQuery,
        mut cursor: Option<&mut LogCursor>,
        cancellation: Option<&dyn Cancellation>,
    ) -> Result<LogPage, BoundaryError> {
        check_cancel(cancellation)?;
        if query.limit == 0 || query.limit > 1000 || query.offset > 1_000_000 {
            return Err(failure());
        }
        let since = query.since.as_deref().map(parse_time).transpose()?;
        let until = query.until.as_deref().map(parse_time).transpose()?;
        if since.zip(until).is_some_and(|(start, end)| start > end) {
            return Err(failure());
        }
        let mut page = LogPage {
            records: vec![],
            next_offset: None,
            malformed_lines: 0,
        };
        let metadata = match fs::symlink_metadata(&self.root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(page),
            Err(_) => return Err(failure()),
        };
        if !metadata.is_dir()
            || metadata.mode() & 0o022 != 0
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(failure());
        }
        let dir = Dir::open_ambient_dir(&self.root, ambient_authority()).map_err(|_| failure())?;
        let opened = dir.dir_metadata().map_err(|_| failure())?;
        if opened.dev() != metadata.dev()
            || opened.ino() != metadata.ino()
            || opened.mode() & 0o022 != 0
            || opened.uid() != unsafe { libc::geteuid() }
        {
            return Err(failure());
        }
        let mut matched = 0;
        let mut files = Vec::new();
        for number in (0..=BACKUPS).rev() {
            check_cancel(cancellation)?;
            let name = if number == 0 {
                "saya.log".into()
            } else {
                format!("saya.log.{number}")
            };
            let Some(file) = open_read(&dir, &name).map_err(|_| failure())? else {
                continue;
            };
            let metadata = file.metadata().map_err(|_| failure())?;
            if metadata.len() > MAX_BYTES {
                return Err(failure());
            }
            files.push((file, metadata));
        }
        if let Some(cursor) = cursor.as_deref_mut() {
            cursor.0.retain(|identity, _| {
                files
                    .iter()
                    .any(|(_, metadata)| *identity == (metadata.dev(), metadata.ino()))
            });
        }
        'files: for (file, metadata) in files {
            let identity = (metadata.dev(), metadata.ino());
            let start = cursor
                .as_deref()
                .and_then(|cursor| cursor.0.get(&identity))
                .map(|position| position.offset)
                .filter(|offset| *offset <= metadata.len())
                .unwrap_or(0);
            if let Some(cursor) = cursor.as_deref_mut() {
                if let std::collections::btree_map::Entry::Vacant(entry) = cursor.0.entry(identity)
                {
                    entry.insert(FollowPosition {
                        offset: start,
                        _file: file.try_clone().map_err(|_| failure())?,
                    });
                }
                if let Some(position) = cursor.0.get_mut(&identity) {
                    position.offset = start;
                }
            }
            let mut reader = BufReader::new(file.take(metadata.len()));
            let mut offset = 0;
            loop {
                check_cancel(cancellation)?;
                let (line, bytes, complete, oversized) =
                    read_line(&mut reader).map_err(|_| failure())?;
                if bytes == 0 {
                    break;
                }
                offset += bytes;
                if offset <= start {
                    continue;
                }
                if !complete {
                    page.malformed_lines += 1;
                    break;
                }
                let entry = if oversized {
                    Err(failure())
                } else {
                    LogEntry::decode(&line, &self.secrets)
                };
                match entry {
                    Ok(entry) if matches_query(&entry, query, since, until) => {
                        if matched >= query.offset {
                            if page.records.len() >= query.limit {
                                page.next_offset = Some(query.offset + page.records.len());
                                break 'files;
                            }
                            page.records.push(entry);
                        }
                        matched += 1;
                    }
                    Ok(_) => {}
                    Err(_) => page.malformed_lines += 1,
                }
                if let Some(cursor) = cursor.as_deref_mut()
                    && let Some(position) = cursor.0.get_mut(&identity)
                {
                    position.offset = offset;
                }
            }
        }
        Ok(page)
    }
}
fn check_cancel(cancellation: Option<&dyn Cancellation>) -> Result<(), BoundaryError> {
    if cancellation.is_some_and(Cancellation::is_cancelled) {
        return Err(crate::contract::BoundaryError(
            crate::contract::ErrorInfo {
                code: "cancelled".into(),
                message: "log query cancelled".into(),
            },
            None,
            None,
        ));
    }
    Ok(())
}
fn open_read(dir: &Dir, name: &str) -> io::Result<Option<fs::File>> {
    let name = std::ffi::CString::new(name).map_err(|_| io::ErrorKind::InvalidInput)?;
    // SAFETY: name is NUL terminated and dir is a live directory descriptor.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(error)
        };
    }
    // SAFETY: this file takes ownership of the newly opened descriptor once.
    let file = unsafe { fs::File::from_raw_fd(fd) };
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(Some(file))
}
fn read_line(reader: &mut impl BufRead) -> io::Result<(Vec<u8>, u64, bool, bool)> {
    let mut line = Vec::new();
    let mut bytes = 0;
    let mut oversized = false;
    loop {
        let mut chunk = Vec::new();
        let count = reader
            .take(MAX_RECORD_BYTES as u64 + 1)
            .read_until(b'\n', &mut chunk)?;
        bytes += count as u64;
        oversized |= line.len().saturating_add(chunk.len()) > MAX_RECORD_BYTES;
        let complete = chunk.last() == Some(&b'\n');
        if !oversized {
            line.extend_from_slice(&chunk);
        }
        if count == 0 || complete {
            return Ok((line, bytes, complete, oversized));
        }
    }
}
fn matches_query(
    entry: &LogEntry,
    query: &LogQuery,
    since: Option<Time>,
    until: Option<Time>,
) -> bool {
    let context = entry.context.as_ref();
    let field = |name: &str| -> Option<&str> {
        match name {
            "session_id" => context
                .and_then(|value| value.session_id.as_deref())
                .or_else(|| entry.data.get(name).and_then(serde_json::Value::as_str)),
            "run_id" => context
                .and_then(|value| value.run_id.as_deref())
                .or_else(|| entry.data.get(name).and_then(serde_json::Value::as_str)),
            _ => None,
        }
    };
    if entry.level < query.minimum
        || query
            .session_id
            .as_deref()
            .is_some_and(|value| field("session_id") != Some(value))
        || query
            .run_id
            .as_deref()
            .is_some_and(|value| field("run_id") != Some(value))
    {
        return false;
    }
    if since.is_none() && until.is_none() {
        return true;
    }
    let Ok(time) = parse_time(&entry.timestamp) else {
        return false;
    };
    since.is_none_or(|start| time >= start) && until.is_none_or(|end| time <= end)
}
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Time(i64, u32);
fn parse_time(value: &str) -> Result<Time, BoundaryError> {
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return Err(failure());
    }
    for range in [0..4, 5..7, 8..10, 11..13, 14..16, 17..19] {
        if !bytes[range].iter().all(u8::is_ascii_digit) {
            return Err(failure());
        }
    }
    let number = |start: usize, end: usize| {
        value
            .get(start..end)
            .ok_or_else(failure)?
            .parse::<i32>()
            .map_err(|_| failure())
    };
    let (year, month, day, hour, minute, second) = (
        number(0, 4)?,
        number(5, 7)?,
        number(8, 10)?,
        number(11, 13)?,
        number(14, 16)?,
        number(17, 19)?,
    );
    if !(1970..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=59).contains(&second)
    {
        return Err(failure());
    }
    let mut position = 19;
    let mut nanos = 0;
    if bytes.get(position) == Some(&b'.') {
        position += 1;
        let start = position;
        while bytes.get(position).is_some_and(u8::is_ascii_digit) {
            position += 1;
        }
        let length = position - start;
        if length == 0 || length > 9 {
            return Err(failure());
        }
        nanos = value[start..position]
            .parse::<u32>()
            .map_err(|_| failure())?
            * 10_u32.pow((9 - length) as u32);
    }
    let zone = value.get(position..).ok_or_else(failure)?;
    let offset = if zone == "Z" {
        0
    } else {
        if zone.len() != 6
            || !matches!(zone.as_bytes()[0], b'+' | b'-')
            || zone.as_bytes()[3] != b':'
        {
            return Err(failure());
        }
        if !zone.as_bytes()[1..3]
            .iter()
            .chain(&zone.as_bytes()[4..6])
            .all(u8::is_ascii_digit)
        {
            return Err(failure());
        }
        let hours = zone
            .get(1..3)
            .ok_or_else(failure)?
            .parse::<i32>()
            .map_err(|_| failure())?;
        let minutes = zone
            .get(4..6)
            .ok_or_else(failure)?
            .parse::<i32>()
            .map_err(|_| failure())?;
        if !(0..=23).contains(&hours) || !(0..=59).contains(&minutes) {
            return Err(failure());
        }
        (hours * 3600 + minutes * 60) * if zone.starts_with('-') { -1 } else { 1 }
    };
    // SAFETY: zeroed tm is valid input for timegm, which normalizes calendar fields.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = year - 1900;
    tm.tm_mon = month - 1;
    tm.tm_mday = day;
    tm.tm_hour = hour;
    tm.tm_min = minute;
    tm.tm_sec = second;
    let seconds = unsafe { libc::timegm(&mut tm) };
    if tm.tm_year != year - 1900 || tm.tm_mon != month - 1 || tm.tm_mday != day {
        return Err(failure());
    }
    Ok(Time(seconds - i64::from(offset), nanos))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::{DiagnosticRecord, JsonLog, LogContext};
    use std::fs::OpenOptions;
    #[test]
    fn paging_filters_legacy_rows_and_refuses_bad_time_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let log = JsonLog::new(dir.path(), vec![]);
        for session in ["first", "second", "first"] {
            let mut record = DiagnosticRecord::new(LogLevel::Warn, "phase");
            record.context = LogContext {
                session_id: Some(session.into()),
                ..Default::default()
            };
            log.record_typed(&record).unwrap();
        }
        log.record("error", "legacy", serde_json::json!({"session_id":"first"}))
            .unwrap();
        let reader = LogReader::new(dir.path(), vec![]);
        let mut query = LogQuery {
            session_id: Some("first".into()),
            limit: 1,
            ..Default::default()
        };
        assert_eq!(reader.page(&query, None).unwrap().next_offset, Some(1));
        query.offset = 2;
        assert_eq!(
            reader.page(&query, None).unwrap().records[0].event,
            "legacy"
        );
        query.since = Some("2026-11-08".into());
        assert!(reader.page(&query, None).is_err());
        assert_eq!(
            parse_time("2026-11-08T08:00:00+08:00").unwrap(),
            parse_time("2026-11-08T00:00:00Z").unwrap()
        );
        assert!(parse_time("2026-02-30T00:00:00Z").is_err());
        for invalid in [
            "2026-11-08T00:00:00++1:00",
            "2026-11-08T00:00:00+00:-1",
            "2026-+1-08T00:00:00Z",
        ] {
            assert!(parse_time(invalid).is_err());
        }
    }
    #[test]
    fn follower_handles_rotation_partial_rows_and_malformed_records() {
        let dir = tempfile::tempdir().unwrap();
        let log = JsonLog::new(dir.path(), vec![]);
        log.record("warn", "first", serde_json::json!({})).unwrap();
        let reader = LogReader::new(dir.path(), vec![]);
        let mut cursor = LogCursor::default();
        let query = LogQuery::default();
        assert_eq!(
            reader
                .follow(&query, &mut cursor, None)
                .unwrap()
                .records
                .len(),
            1
        );
        std::fs::rename(dir.path().join("saya.log"), dir.path().join("saya.log.1")).unwrap();
        log.record("error", "second", serde_json::json!({}))
            .unwrap();
        let mut file = OpenOptions::new()
            .append(true)
            .open(dir.path().join("saya.log"))
            .unwrap();
        use std::io::Write;
        file.write_all(b"broken\n{\"timestamp\":").unwrap();
        let page = reader.follow(&query, &mut cursor, None).unwrap();
        assert_eq!(page.records.len(), 1);
        assert_eq!(page.records[0].event, "second");
        assert_eq!(page.malformed_lines, 2);
        file.write_all(
            b"\"2026-11-08T00:00:00Z\",\"level\":\"warn\",\"event\":\"third\",\"data\":{}}\n",
        )
        .unwrap();
        let page = reader.follow(&query, &mut cursor, None).unwrap();
        assert_eq!(page.records.len(), 1);
        assert_eq!(page.records[0].event, "third");
    }
    #[test]
    fn rapid_file_replacement_keeps_generations_and_oversized_rows_bounded() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let log = JsonLog::new(dir.path(), vec![]);
        let reader = LogReader::new(dir.path(), vec![]);
        let mut cursor = LogCursor::default();
        for number in 0..100 {
            let name = format!("generation-{number}");
            log.record("warn", &name, serde_json::json!({})).unwrap();
            let page = reader
                .follow(&LogQuery::default(), &mut cursor, None)
                .unwrap();
            assert_eq!(page.records.len(), 1);
            assert_eq!(page.records[0].event, name);
            assert!(cursor.0.len() <= BACKUPS + 1);
            std::fs::remove_file(dir.path().join("saya.log")).unwrap();
        }
        log.record("warn", "after", serde_json::json!({})).unwrap();
        let mut file = OpenOptions::new()
            .append(true)
            .open(dir.path().join("saya.log"))
            .unwrap();
        file.write_all(&vec![b'x'; MAX_RECORD_BYTES * 2]).unwrap();
        file.write_all(b"\n").unwrap();
        let page = reader.page(&LogQuery::default(), None).unwrap();
        assert_eq!(page.records.len(), 1);
        assert_eq!(page.malformed_lines, 1);
    }
    #[test]
    fn queries_are_read_only_private_and_cancellable() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        assert!(
            LogReader::new(&missing, vec![])
                .page(&LogQuery::default(), None)
                .unwrap()
                .records
                .is_empty()
        );
        assert!(!missing.exists());
        struct Cancelled;
        impl Cancellation for Cancelled {
            fn is_cancelled(&self) -> bool {
                true
            }
            fn cancelled(
                &self,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
                Box::pin(async {})
            }
        }
        assert_eq!(
            LogReader::new(&missing, vec![])
                .page(&LogQuery::default(), Some(&Cancelled))
                .unwrap_err()
                .0
                .code,
            "cancelled"
        );
        let target = tempfile::NamedTempFile::new().unwrap();
        std::os::unix::fs::symlink(target.path(), dir.path().join("saya.log")).unwrap();
        assert!(
            LogReader::new(dir.path(), vec![])
                .page(&LogQuery::default(), None)
                .is_err()
        );
    }
}
