//! Close-on-exec normalization without allocation or descriptor discovery races
//! after fork. Stdio is owned by the caller. Bwrap descriptors are re-declared
//! after this pass; cwd and cgroup descriptors stay usable until exec.
use std::io;
#[cfg(target_os = "macos")]
use std::mem::MaybeUninit;

pub(super) struct DescriptorSanitizer {
    #[cfg(target_os = "macos")]
    entries: Vec<MaybeUninit<libc::proc_fdinfo>>,
}
impl DescriptorSanitizer {
    pub(super) fn prepare() -> io::Result<Self> {
        #[cfg(target_os = "macos")]
        {
            let mut entries = Vec::new();
            entries
                .try_reserve_exact(65_536)
                .map_err(|_| io::Error::from_raw_os_error(libc::ENOMEM))?;
            entries.resize_with(65_536, MaybeUninit::uninit);
            Ok(Self { entries })
        }
        #[cfg(target_os = "linux")]
        {
            Ok(Self {})
        }
    }
    pub(super) fn mark_close_on_exec(&mut self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            // SAFETY: close_range with CLOEXEC only updates the forked child's
            // descriptor flags, leaving stdio and all descriptors usable until exec.
            if unsafe {
                libc::syscall(
                    libc::SYS_close_range,
                    3u32,
                    u32::MAX,
                    libc::CLOSE_RANGE_CLOEXEC,
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
        #[cfg(target_os = "macos")]
        {
            let capacity =
                (self.entries.len() * std::mem::size_of::<libc::proc_fdinfo>()) as libc::c_int;
            // Apple's libproc wrapper directly invokes __proc_info. Storage is
            // allocated before fork; the single-threaded child table is stable.
            // SAFETY: pid is our child, the buffer holds capacity writable bytes.
            let pid = unsafe { libc::getpid() };
            let needed = unsafe {
                libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0)
            };
            if needed <= 0 {
                return Err(io::Error::last_os_error());
            }
            if needed > capacity {
                return Err(io::Error::from_raw_os_error(libc::EMFILE));
            }
            let bytes = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDLISTFDS,
                    0,
                    self.entries.as_mut_ptr().cast(),
                    capacity,
                )
            };
            if bytes <= 0 {
                return Err(io::Error::last_os_error());
            }
            if bytes > capacity
                || !(bytes as usize).is_multiple_of(std::mem::size_of::<libc::proc_fdinfo>())
            {
                return Err(io::Error::from_raw_os_error(libc::EMFILE));
            }
            for entry in self
                .entries
                .iter()
                .take(bytes as usize / std::mem::size_of::<libc::proc_fdinfo>())
            {
                // SAFETY: proc_pidinfo initialized exactly the returned entries.
                let fd = unsafe { entry.assume_init_ref() }.proc_fd;
                if fd < 3 {
                    continue;
                }
                // SAFETY: fd was enumerated in this single-threaded child.
                let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
                if flags < 0 {
                    return Err(io::Error::last_os_error());
                }
                if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        }
    }
}

pub(super) fn inherit_descriptor(file: &std::fs::File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: the file owns this valid descriptor, changed only in the forked child.
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
    if flags < 0
        || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
