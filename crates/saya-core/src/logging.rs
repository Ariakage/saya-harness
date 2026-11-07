//! Local structured warning/error logs with bounded retention and redaction.
use crate::{
    contract::{BoundaryError, ErrorInfo},
    redaction,
};
use cap_std::{ambient_authority, fs::Dir};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_BYTES: u64 = 10 * 1024 * 1024;
const BACKUPS: usize = 5;
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
}
impl JsonLog {
    pub fn new(root: impl Into<PathBuf>, secrets: Vec<String>) -> Self {
        Self {
            root: root.into(),
            secrets,
            max_bytes: MAX_BYTES,
            backups: BACKUPS,
        }
    }
    /// Append one warning or error after redaction. Locking serializes writers
    /// across processes; I/O and unsafe paths return a bounded error.
    pub fn record(&self, level: &str, event: &str, data: Value) -> Result<(), BoundaryError> {
        if !matches!(level, "warn" | "error") {
            return Err(failure());
        }
        let mut record = json!({"timestamp":timestamp()?,"level":level,"event":event,"data":data});
        redaction::redact(&mut record, &self.secrets).map_err(|_| failure())?;
        let mut line = serde_json::to_vec(&record).map_err(|_| failure())?;
        line.push(b'\n');
        if u64::try_from(line.len()).map_err(|_| failure())? > self.max_bytes {
            return Err(failure());
        }
        let dir = open_root(&self.root).map_err(|_| failure())?;
        let lock = open_private(&dir, "saya-log.lock").map_err(|_| failure())?;
        loop {
            // SAFETY: lock is a live file descriptor and flock retains no pointer.
            if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } == 0 {
                break;
            }
            if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                return Err(failure());
            }
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
    fs::create_dir_all(root)?;
    if !fs::symlink_metadata(root)?.file_type().is_dir() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Dir::open_ambient_dir(root, ambient_authority())
}
fn validate_existing(dir: &Dir, name: &str) -> io::Result<()> {
    match dir.symlink_metadata(name) {
        Ok(meta) if meta.file_type().is_file() => Ok(()),
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
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.mode() & 0o077 != 0 {
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
