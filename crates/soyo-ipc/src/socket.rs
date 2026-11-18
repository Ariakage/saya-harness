//! Helper endpoint resolution (socket path / pipe name, pid file).

use std::path::PathBuf;

/// Local socket name shared by all helpers (`~/.soyo/helper.sock` on Unix,
/// `soyo-helper` named pipe on Windows).
pub fn helper_socket_name() -> interprocess::local_socket::Name<'static> {
    #[cfg(unix)]
    {
        helper_socket_name_at(helper_dir().join("helper.sock"))
    }
    #[cfg(windows)]
    {
        interprocess::local_socket::ToNsName::to_ns_name("soyo-helper")
            .expect("helper pipe name should be valid")
    }
}

/// Filesystem socket name for an explicit path (tests use temp dirs).
#[cfg(unix)]
pub fn helper_socket_name_at(path: PathBuf) -> interprocess::local_socket::Name<'static> {
    interprocess::local_socket::ToFsName::to_fs_name::<interprocess::local_socket::GenericFilePath>(
        path,
    )
    .expect("helper socket name should be valid")
}

/// Pid file next to the socket, used by `soyo kill` as a fallback.
pub fn helper_pid_path() -> PathBuf {
    helper_dir().join("helper.pid")
}

/// `~/.soyo` — the shared data root; created on demand by the helper.
/// `SOYO_HELPER_DIR` overrides the directory (tests and multi-instance dev).
pub fn helper_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("SOYO_HELPER_DIR")
        && !dir.trim().is_empty()
    {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .unwrap_or_else(|| ".".to_owned());
    PathBuf::from(home).join(".soyo")
}

/// Which helper address to connect to — the canonical socket name for now.
pub struct HelperEndpoint;

impl HelperEndpoint {
    pub fn name() -> interprocess::local_socket::Name<'static> {
        helper_socket_name()
    }

    /// Filesystem path of the socket on Unix; `None` on Windows (named pipe).
    pub fn socket_path() -> Option<PathBuf> {
        #[cfg(unix)]
        {
            Some(helper_dir().join("helper.sock"))
        }
        #[cfg(windows)]
        {
            None
        }
    }
}
