//! Optional resource control confined to a caller-delegated cgroup v2 subtree.
#[cfg(target_os = "linux")]
use super::failure;
use super::{ResourceLimits, unavailable};
use crate::contract::BoundaryError;
use std::{
    fs, io,
    path::{Path, PathBuf},
};
#[cfg(target_os = "linux")]
use std::{
    os::fd::AsRawFd,
    time::{Duration, Instant},
};

pub(super) struct Cgroup {
    path: PathBuf,
}
impl Cgroup {
    pub(super) fn prepare(
        root: Option<&Path>,
        limits: &ResourceLimits,
        writable: &[PathBuf],
    ) -> Result<Option<Self>, BoundaryError> {
        let required = limits.total_memory_bytes.is_some()
            || limits.total_processes.is_some()
            || limits.cpu_quota.is_some();
        let Some(root) = root else {
            return if required {
                Err(unavailable(
                    "aggregate resources require an explicitly delegated cgroup v2 root",
                ))
            } else {
                Ok(None)
            };
        };
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (root, writable);
            Err(unavailable("cgroup v2 is unavailable on this platform"))
        }
        #[cfg(target_os = "linux")]
        {
            let root = fs::canonicalize(root).map_err(super::io_error)?;
            if writable
                .iter()
                .any(|path| root.starts_with(path) || path.starts_with(&root))
            {
                return Err(failure(
                    "invalid_sandbox_policy",
                    "cgroup control tree must be outside writable roots",
                ));
            }
            let directory = fs::File::open(&root).map_err(super::io_error)?;
            // SAFETY: the fd is open and stat is valid writable storage.
            let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
            if unsafe { libc::fstatfs(directory.as_raw_fd(), &mut stat) } != 0 {
                return Err(super::io_error(io::Error::last_os_error()));
            }
            if stat.f_type as u64 != 0x6367_7270 {
                return Err(unavailable("delegated root is not a cgroup v2 filesystem"));
            }
            let path = root.join(format!("saya-{}", uuid::Uuid::now_v7()));
            fs::create_dir(&path)
                .map_err(|_| unavailable("cgroup root is not writable or delegated"))?;
            let group = Self { path };
            for name in [
                "memory.max",
                "pids.max",
                "cpu.max",
                "cgroup.kill",
                "cgroup.events",
                "cgroup.procs",
            ] {
                if !group.path.join(name).is_file() {
                    return Err(unavailable(format!("delegated cgroup lacks {name}")));
                }
            }
            for (name, value) in [
                ("memory.max", limits.total_memory_bytes),
                ("pids.max", limits.total_processes),
            ] {
                if let Some(value) = value {
                    group.write_checked(name, &value.to_string())?;
                }
            }
            if let Some(quota) = &limits.cpu_quota {
                group.write_checked(
                    "cpu.max",
                    &format!("{} {}", quota.quota_micros, quota.period_micros),
                )?;
            }
            // Validate delegation before any caller payload starts.
            group.join_descriptor().map_err(|_| {
                unavailable("cgroup.procs is not writable in the delegated subtree")
            })?;
            fs::OpenOptions::new()
                .write(true)
                .open(group.path.join("cgroup.kill"))
                .map_err(|_| unavailable("delegated cgroup cannot kill owned descendants"))?;
            Ok(Some(group))
        }
    }
    #[cfg(target_os = "linux")]
    fn write_checked(&self, name: &str, value: &str) -> Result<(), BoundaryError> {
        use std::io::Write;
        let path = self.path.join(name);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .map_err(|_| unavailable(format!("delegated controller {name} is not writable")))?;
        file.write_all(value.as_bytes()).map_err(super::io_error)?;
        let actual = fs::read_to_string(path).map_err(super::io_error)?;
        if actual.split_whitespace().collect::<Vec<_>>()
            != value.split_whitespace().collect::<Vec<_>>()
        {
            return Err(unavailable(format!(
                "controller {name} did not retain the requested limit"
            )));
        }
        Ok(())
    }
    pub(super) fn join_descriptor(&self) -> io::Result<fs::File> {
        fs::OpenOptions::new()
            .write(true)
            .open(self.path.join("cgroup.procs"))
    }
    pub(super) fn cleanup(&self) -> Result<(), BoundaryError> {
        #[cfg(not(target_os = "linux"))]
        {
            Err(unavailable("cgroup v2 is unavailable on this platform"))
        }
        #[cfg(target_os = "linux")]
        {
            match fs::write(self.path.join("cgroup.kill"), b"1") {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(failure("sandbox_cleanup_failed", error.to_string())),
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let events =
                    fs::read_to_string(self.path.join("cgroup.events")).map_err(super::io_error)?;
                if events.lines().any(|line| line == "populated 0") {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(failure(
                        "sandbox_cleanup_failed",
                        "cgroup remained populated after kill",
                    ));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            fs::remove_dir(&self.path)
                .map_err(|error| failure("sandbox_cleanup_failed", error.to_string()))
        }
    }
}
impl Drop for Cgroup {
    fn drop(&mut self) {
        if self.cleanup().is_err() {
            let _ = fs::remove_dir(&self.path);
        }
    }
}
/// Write the current child PID without allocating after fork.
pub(super) fn join(file: &fs::File) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        let bytes = b"0\n";
        // SAFETY: descriptor and buffer are valid for this synchronous write.
        if unsafe { libc::write(file.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) }
            != bytes.len() as isize
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = file;
        Err(io::Error::from_raw_os_error(libc::ENOTSUP))
    }
}
