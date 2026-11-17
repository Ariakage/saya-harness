//! Bubblewrap filesystem views with a base filter and payload-only socket filter.
use super::{
    BrowserSandboxDirectories, BrowserSandboxProfile, NetworkPolicy, PreparedCommand,
    SandboxPolicy, browser_unavailable, failure, loopback::LoopbackRelay, unavailable,
    worker::NamespaceBridge,
};
use crate::contract::BoundaryError;
use std::{
    collections::BTreeMap,
    ffi::{CStr, OsString},
    fs,
    io::{self, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, MetadataExt, PermissionsExt},
        },
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

mod seccomp;
#[cfg(test)]
mod tests;

#[cfg(test)]
#[allow(unused_imports)]
pub(in crate::execution::linux) use seccomp::{
    ALLOW, AND, BITS, DENY, EQUAL, LOAD, RETURN, instruction,
};
pub(in crate::execution) use seccomp::{Filter, inherit_filter_descriptor, install_payload_filter};
pub(in crate::execution::linux) use seccomp::{browser_filter, filter};

pub(super) struct LinuxBackend {
    bwrap: PathBuf,
    socat: Option<BridgeProgram>,
    bridges: Vec<NamespaceBridge>,
    relay: Mutex<Option<LoopbackRelay>>,
    args: Vec<OsString>,
    filter_path: PathBuf,
    mount_fds: Vec<Arc<fs::File>>,
    helper_path: PathBuf,
}
struct BridgeProgram {
    path: PathBuf,
    device: u64,
    inode: u64,
}
impl BridgeProgram {
    fn pin(source: &Path, scratch: &Path) -> io::Result<(Self, Arc<fs::File>)> {
        let source = fs::File::open(source)?;
        let metadata = source.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != 0
            || metadata.mode() & 0o022 != 0
            || metadata.mode() & 0o111 == 0
        {
            return Err(io::Error::from_raw_os_error(libc::EPERM));
        }
        let path = scratch.join("saya-socat");
        super::transport::copy_pinned_image(
            &source,
            super::transport::fingerprint(&metadata),
            &path,
        )?;
        let file = Arc::new(fs::File::open(&path)?);
        let metadata = file.metadata()?;
        Ok((
            Self {
                path,
                device: metadata.dev(),
                inode: metadata.ino(),
            },
            file,
        ))
    }
}
const SYSTEM_READ_ROOTS: [&str; 6] = [
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib64",
    "/etc/alternatives",
];
// Browser workers additionally need runtime libraries, fonts and certificate
// stores. Whole /etc, HOME, workspaces and credential trees stay unbound.
const BROWSER_SYSTEM_READ_ROOTS: [&str; 18] = [
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib64",
    "/etc/alternatives",
    "/etc/ssl",
    "/etc/pki",
    "/etc/ca-certificates",
    "/etc/fonts",
    "/etc/localtime",
    "/etc/resolv.conf",
    "/etc/hosts",
    "/etc/nsswitch.conf",
    "/etc/ld.so.cache",
    "/etc/ld.so.conf",
    "/etc/ld.so.conf.d",
    "/etc/host.conf",
];
impl LinuxBackend {
    pub(super) fn helper_path(&self) -> &Path {
        &self.helper_path
    }
    pub(super) fn shutdown_relay(&self) -> Result<(), BoundaryError> {
        let mut relay = self.relay.lock().map_err(|_| {
            failure(
                "sandbox_cleanup_unknown",
                "loopback relay cleanup lock is poisoned",
            )
        })?;
        if let Some(active) = relay.as_mut() {
            active.shutdown().map_err(|_| {
                failure(
                    "sandbox_cleanup_unknown",
                    "loopback relay shutdown could not be confirmed",
                )
            })?;
        }
        *relay = None;
        Ok(())
    }
    pub(super) fn prepare(policy: &SandboxPolicy, scratch: &Path) -> Result<Self, BoundaryError> {
        Self::prepare_with_profile(policy, scratch, None)
    }
    /// Browser workers need nested user namespaces, a private procfs and
    /// /dev/shm while keeping the generic payload filter untouched.
    pub(super) fn prepare_browser(
        policy: &SandboxPolicy,
        profile: &BrowserSandboxProfile,
        directories: &BrowserSandboxDirectories,
        scratch: &Path,
    ) -> Result<Self, BoundaryError> {
        Self::prepare_with_profile(policy, scratch, Some((profile, directories)))
    }
    fn prepare_with_profile(
        policy: &SandboxPolicy,
        scratch: &Path,
        browser: Option<(&BrowserSandboxProfile, &BrowserSandboxDirectories)>,
    ) -> Result<Self, BoundaryError> {
        if let Some((profile, _)) = browser {
            reject_browser_install_roots(profile)?;
            verify_nested_user_namespace()?;
        } else {
            reject_control_filesystem_roots(policy)?;
        }
        verify_supported_pair_kernel()?;
        verify_connected_pair_boundaries(scratch).map_err(|error| {
            unavailable(format!(
                "host kernel anonymous Unix socket-pair boundaries could not be verified ({error})"
            ))
        })?;
        let bwrap = trusted_program(&["/usr/local/bin/bwrap", "/usr/bin/bwrap", "/bin/bwrap"])?;
        let version = Command::new(&bwrap)
            .arg("--version")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null())
            .output()
            .map_err(super::io_error)?;
        let version_text = String::from_utf8_lossy(&version.stdout);
        if !version.status.success() || !supported_version(&version_text) {
            return Err(unavailable(
                "Bubblewrap 0.13.1 or newer is required for the validated filesystem setup and descriptor APIs",
            ));
        }
        let browser_mode = browser.is_some();
        let filter_path = scratch.join(if browser_mode {
            "browser.bpf"
        } else {
            "base.bpf"
        });
        let mut filter_file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&filter_path)
            .map_err(super::io_error)?;
        let base_program = if browser_mode {
            browser_filter(false)
        } else {
            filter(false)
        };
        for instruction in base_program {
            filter_file
                .write_all(&instruction.code.to_ne_bytes())
                .map_err(super::io_error)?;
            filter_file
                .write_all(&[instruction.jt, instruction.jf])
                .map_err(super::io_error)?;
            filter_file
                .write_all(&instruction.k.to_ne_bytes())
                .map_err(super::io_error)?;
        }
        drop(filter_file);
        fs::set_permissions(&filter_path, fs::Permissions::from_mode(0o400))
            .map_err(super::io_error)?;
        let filter = Arc::new(fs::File::open(&filter_path).map_err(super::io_error)?);
        let mut args: Vec<OsString> = [
            "--unshare-user",
            "--unshare-pid",
            "--unshare-ipc",
            "--unshare-net",
            "--unshare-uts",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        if browser_mode {
            // Chromium refuses to start as euid 0. Map a non-root identity
            // inside the user namespace; the host uid remains the only mapped
            // identity, so private directories stay writable and host
            // authority is unchanged.
            args.extend(["--uid", "65534", "--gid", "65534"].map(Into::into));
        } else {
            args.push("--disable-userns".into());
        }
        args.extend(
            [
                "--new-session",
                "--die-with-parent",
                "--cap-drop",
                "ALL",
                "--dev",
                "/dev",
            ]
            .into_iter()
            .map(Into::into),
        );
        if browser_mode {
            // Chromium's own sandbox needs a private procfs and a writable
            // shared-memory filesystem. The generic view deliberately omits
            // /proc so this stays browser-only.
            args.extend(
                ["--proc", "/proc", "--perms", "1777", "--tmpfs", "/dev/shm"].map(Into::into),
            );
        }
        if browser_mode {
            args.extend(["--perms", "1777", "--tmpfs", "/tmp"].map(Into::into));
        } else {
            args.extend(["--tmpfs", "/tmp"].into_iter().map(Into::into));
        }
        let mut mount_fds = Vec::new();
        let mut system_roots = Vec::new();
        let system_read_roots: &[&str] = if browser_mode {
            &BROWSER_SYSTEM_READ_ROOTS
        } else {
            &SYSTEM_READ_ROOTS
        };
        for path in system_read_roots {
            if Path::new(path).exists() {
                system_roots.push((
                    fs::canonicalize(path).map_err(super::io_error)?,
                    PathBuf::from(path),
                ));
                bind(
                    &mut args,
                    &mut mount_fds,
                    "--ro-bind-fd",
                    Path::new(path),
                    Path::new(path),
                )?;
            }
        }
        if let Some((profile, directories)) = browser {
            for path in &profile.install_roots {
                bind(&mut args, &mut mount_fds, "--ro-bind-fd", path, path)?;
            }
            for path in [
                &directories.profile,
                &directories.output,
                &directories.tmp,
                &directories.work,
                &directories.config,
            ] {
                bind(&mut args, &mut mount_fds, "--bind-fd", path, path)?;
            }
        } else {
            for path in &policy.read_roots {
                bind(&mut args, &mut mount_fds, "--ro-bind-fd", path, path)?;
            }
            for path in &policy.read_files {
                bind(&mut args, &mut mount_fds, "--ro-bind-fd", path, path)?;
            }
            for path in &policy.write_roots {
                bind(&mut args, &mut mount_fds, "--bind-fd", path, path)?;
            }
        }
        if policy.synthetic_etc_identity
            || matches!(policy.network, NetworkPolicy::LoopbackPort { .. })
        {
            // SAFETY: numeric identity queries have no side effects.
            let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
            for (name, contents) in [
                (
                    "passwd",
                    format!("saya:x:{uid}:{gid}::/nonexistent:/bin/false\n"),
                ),
                ("group", format!("saya:x:{gid}:\n")),
                (
                    "nsswitch.conf",
                    "passwd: files\ngroup: files\nhosts: files\n".into(),
                ),
            ] {
                let path = scratch.join(format!("account-{name}"));
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                    .map_err(super::io_error)?;
                file.write_all(contents.as_bytes())
                    .map_err(super::io_error)?;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o400))
                    .map_err(super::io_error)?;
                bind(
                    &mut args,
                    &mut mount_fds,
                    "--ro-bind-fd",
                    &path,
                    &PathBuf::from("/etc").join(name),
                )?;
            }
        }
        let helper_path = scratch.join("saya-worker");
        // /proc/self/exe pins the image actually running, including when its
        // original workspace pathname has been replaced or unlinked.
        super::transport::copy_launcher(&helper_path).map_err(super::io_error)?;
        fs::set_permissions(&helper_path, fs::Permissions::from_mode(0o500))
            .map_err(super::io_error)?;
        bind(
            &mut args,
            &mut mount_fds,
            "--ro-bind-fd",
            &helper_path,
            &helper_path,
        )?;
        // Masks live only in the private filesystem view. Missing names under a
        // writable root cannot safely be masked without altering the host tree.
        if !browser_mode {
            let empty = scratch.join("empty");
            fs::create_dir(&empty).map_err(super::io_error)?;
            let mask = scratch.join("denied");
            fs::write(&mask, b"").map_err(super::io_error)?;
            let mask_file = Arc::new(fs::File::open(&mask).map_err(super::io_error)?);
            fs::set_permissions(&mask, fs::Permissions::from_mode(0o000))
                .map_err(super::io_error)?;
            for (source, destination) in protection_masks(policy, &system_roots)? {
                match fs::symlink_metadata(&source) {
                    Ok(metadata) => {
                        if metadata.is_dir() {
                            bind(
                                &mut args,
                                &mut mount_fds,
                                "--ro-bind-fd",
                                &empty,
                                &destination,
                            )?;
                        } else {
                            // Bubblewrap consumes each bind-fd descriptor. Aliases
                            // of the same mask need independently owned fd numbers.
                            bind_descriptor(
                                &mut args,
                                &mut mount_fds,
                                "--ro-bind-fd",
                                Arc::new(mask_file.try_clone().map_err(super::io_error)?),
                                &destination,
                            );
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        if policy
                            .write_roots
                            .iter()
                            .any(|root| destination.starts_with(root))
                        {
                            return Err(failure(
                                "unsupported_fs_policy",
                                "a missing protected path inside a writable root cannot be safely masked",
                            ));
                        }
                    }
                    Err(error) => return Err(super::io_error(error)),
                }
            }
            // Write-only denials remount each existing target read-only over
            // itself. A missing target inside a writable root is covered by
            // the empty mask so it cannot be created later.
            for (source, destination) in write_deny_masks(policy, &system_roots)? {
                match fs::symlink_metadata(&source) {
                    Ok(_) => {
                        bind(
                            &mut args,
                            &mut mount_fds,
                            "--ro-bind-fd",
                            &source,
                            &destination,
                        )?;
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        if policy
                            .write_roots
                            .iter()
                            .any(|root| destination.starts_with(root))
                        {
                            bind_descriptor(
                                &mut args,
                                &mut mount_fds,
                                "--ro-bind-fd",
                                Arc::new(mask_file.try_clone().map_err(super::io_error)?),
                                &destination,
                            );
                        }
                    }
                    Err(error) => return Err(super::io_error(error)),
                }
            }
        }
        let mut relay = None;
        let bridges = match &policy.network {
            NetworkPolicy::Deny => Vec::new(),
            NetworkPolicy::Proxy(proxy) => {
                if proxy.endpoint.ip() != std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST) {
                    return Err(unavailable("Linux proxy bridges require IPv4 loopback"));
                }
                let socket = proxy.unix_socket.clone().ok_or_else(|| {
                    unavailable("Linux proxy isolation requires a private Unix socket")
                })?;
                vec![NamespaceBridge {
                    socket,
                    endpoint: proxy.endpoint,
                }]
            }
            NetworkPolicy::LoopbackPort {
                host,
                port,
                proxy_port,
            } => {
                let mut endpoints = vec![std::net::SocketAddr::new(*host, *port)];
                if let Some(proxy) = proxy_port {
                    endpoints.push(std::net::SocketAddr::from(([127, 0, 0, 1], *proxy)));
                }
                let active = LoopbackRelay::start(scratch, &endpoints)
                    .map_err(|_| unavailable("private loopback relays could not be started"))?;
                let bridges = active.bridges().to_vec();
                relay = Some(active);
                bridges
            }
        };
        for bridge in &bridges {
            let path = &bridge.socket;
            if !safe_socat_path(path) {
                return Err(failure(
                    "invalid_sandbox_policy",
                    "Unix bridge path contains unsupported socat address characters",
                ));
            }
            let metadata = fs::metadata(path).map_err(super::io_error)?;
            use std::os::unix::fs::FileTypeExt;
            if !metadata.file_type().is_socket() {
                return Err(failure(
                    "invalid_sandbox_policy",
                    "bridge path must be a Unix socket",
                ));
            }
            if policy.write_roots.iter().any(|root| path.starts_with(root)) {
                return Err(failure(
                    "invalid_sandbox_policy",
                    "bridge socket must be outside writable roots",
                ));
            }
            args.extend([
                OsString::from("--ro-bind"),
                path.as_os_str().to_owned(),
                path.as_os_str().to_owned(),
            ]);
        }
        let socat = if bridges.is_empty() {
            None
        } else {
            let source =
                trusted_program(&["/usr/bin/socat", "/bin/socat", "/usr/local/bin/socat"])?;
            let (program, file) = BridgeProgram::pin(&source, scratch)
                .map_err(|_| unavailable("trusted socat program could not be pinned"))?;
            bind_descriptor(
                &mut args,
                &mut mount_fds,
                "--ro-bind-fd",
                file,
                &program.path,
            );
            Some(program)
        };
        let backend = Self {
            bwrap,
            socat,
            bridges,
            relay: Mutex::new(relay),
            args,
            filter_path,
            mount_fds,
            helper_path,
        };
        let mut probe = Command::new(&backend.bwrap);
        probe
            .args(&backend.args)
            .arg("--seccomp")
            .arg(filter.as_raw_fd().to_string())
            .arg("/usr/bin/true")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let descriptor = filter;
        let mount_fds = backend.mount_fds.clone();
        // SAFETY: descriptor inheritance uses only fcntl and prctl after fork.
        unsafe {
            probe.pre_exec(move || {
                inherit_filter_descriptor(&descriptor)?;
                for fd in &mount_fds {
                    inherit_filter_descriptor(fd)?;
                }
                Ok(())
            });
        }
        use std::os::unix::process::CommandExt;
        let status = probe
            .status()
            .map_err(|_| unavailable("Bubblewrap namespace/Seccomp probe failed to start"))?;
        if !status.success() {
            return Err(unavailable(
                "Bubblewrap cannot establish required user, mount, PID and network namespaces or Seccomp",
            ));
        }
        Ok(backend)
    }
    pub(super) fn wrap(
        &self,
        command: &mut PreparedCommand,
        argv: &[OsString],
    ) -> Result<(), BoundaryError> {
        if !Path::new(&argv[0]).is_absolute() {
            return Err(failure(
                "invalid_args",
                "sandbox command executable must be absolute",
            ));
        }
        let descriptor = Arc::new(fs::File::open(&self.filter_path).map_err(super::io_error)?);
        command.program = self.bwrap.clone();
        command.args = self.args.clone();
        command.args.push("--seccomp".into());
        command.args.push(descriptor.as_raw_fd().to_string().into());
        command.args.push("--chdir".into());
        command.args.push(command.cwd.clone().into_os_string());
        command.args.push(self.helper_path.clone().into_os_string());
        command.args.push("__saya-sandbox-launch".into());
        if !self.bridges.is_empty() {
            let socat = self
                .socat
                .as_ref()
                .ok_or_else(|| unavailable("socat is unavailable"))?;
            let bridges = serde_json::to_string(&self.bridges)
                .map_err(|_| failure("invalid_sandbox_policy", "bridge list cannot be encoded"))?;
            if bridges.len() > 16 * 1024 {
                return Err(failure(
                    "invalid_sandbox_policy",
                    "bridge list exceeds the launcher limit",
                ));
            }
            command.args.extend([
                OsString::from("--socat"),
                socat.path.clone().into_os_string(),
                OsString::from("--socat-identity"),
                format!("{}:{}", socat.device, socat.inode).into(),
                OsString::from("--bridges"),
                bridges.into(),
            ]);
        }
        command.bootstrap_index = Some(command.args.len());
        command.args.push("--".into());
        let mut payload = argv.to_vec();
        if Path::new(&payload[0]) == std::env::current_exe().map_err(super::io_error)?
            && payload
                .get(1)
                .is_some_and(|value| value == "__saya-file-worker")
        {
            payload[0] = self.helper_path.clone().into_os_string();
        }
        command.args.extend(payload);
        command.seccomp = Some(descriptor);
        command.mount_fds = self.mount_fds.clone();
        Ok(())
    }
}

// A descriptor bind of a symlinked system root creates a separate guest view.
// Apply each protection to all aliases, including a root wholly below a deny.
fn protection_masks(
    policy: &SandboxPolicy,
    system_roots: &[(PathBuf, PathBuf)],
) -> Result<Vec<(PathBuf, PathBuf)>, BoundaryError> {
    const MAX_MASKS: usize = 4096;
    let mut masks = BTreeMap::new();
    for protected in &policy.protected_paths {
        for root in policy.read_roots.iter().chain(&policy.write_roots) {
            if protected.starts_with(root) {
                masks.insert(protected.clone(), protected.clone());
            } else if root.starts_with(protected) {
                masks.insert(root.clone(), root.clone());
            }
        }
        for (source, destination) in system_roots {
            if let Ok(relative) = protected.strip_prefix(source) {
                masks.insert(destination.join(relative), protected.clone());
            } else if source.starts_with(protected) {
                masks.insert(destination.clone(), source.clone());
            }
        }
        if masks.len() > MAX_MASKS {
            return Err(failure(
                "unsupported_fs_policy",
                "filesystem protection masks exceed their budget",
            ));
        }
    }
    let mut result: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (destination, source) in masks {
        if result
            .last()
            .is_some_and(|(_, parent)| destination.starts_with(parent))
        {
            continue;
        }
        result.push((source, destination));
    }
    Ok(result)
}

// Write-only denials keep the target visible but remount it read-only. Only
// writable roots need masks; read-only views already reject writes. System
// root aliases receive the same coverage as the full protection masks.
fn write_deny_masks(
    policy: &SandboxPolicy,
    system_roots: &[(PathBuf, PathBuf)],
) -> Result<Vec<(PathBuf, PathBuf)>, BoundaryError> {
    const MAX_MASKS: usize = 4096;
    let mut masks = BTreeMap::new();
    for denied in &policy.write_denied_paths {
        for root in &policy.write_roots {
            if denied.starts_with(root) {
                masks.insert(denied.clone(), denied.clone());
            }
        }
        for (source, destination) in system_roots {
            if let Ok(relative) = denied.strip_prefix(source) {
                masks.insert(destination.join(relative), denied.clone());
            }
        }
        if masks.len() > MAX_MASKS {
            return Err(failure(
                "unsupported_fs_policy",
                "filesystem write-denial masks exceed their budget",
            ));
        }
    }
    let mut result: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (destination, source) in masks {
        if result
            .last()
            .is_some_and(|(_, parent)| destination.starts_with(parent))
        {
            continue;
        }
        result.push((source, destination));
    }
    Ok(result)
}

fn verify_supported_pair_kernel() -> Result<(), BoundaryError> {
    // SAFETY: uname fills the complete fixed-size utsname structure and
    // guarantees NUL-terminated release text on success.
    let mut identity: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut identity) } != 0 {
        return Err(unavailable(
            "Linux kernel socket-pair fixes could not be identified",
        ));
    }
    let release = unsafe { CStr::from_ptr(identity.release.as_ptr()) };
    if !release.to_str().is_ok_and(known_fixed_pair_kernel_release) {
        return Err(unavailable(
            "Linux socket-pair isolation requires a known-fixed kernel release: 5.15.86+, 6.1.2+, or 6.2+; unknown vendor backports and release candidates are unsupported",
        ));
    }
    Ok(())
}

fn known_fixed_pair_kernel_release(release: &str) -> bool {
    if release.to_ascii_lowercase().contains("-rc") {
        return false;
    }
    let Some(core) = release.split(['-', '+']).next() else {
        return false;
    };
    let version = core
        .split('.')
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>();
    let Ok(version) = version else {
        return false;
    };
    let [major, minor, patch] = version.as_slice() else {
        return false;
    };
    (*major == 5 && *minor == 15 && *patch >= 86)
        || (*major == 6 && *minor == 1 && *patch >= 2)
        || (*major, *minor) >= (6, 2)
}

// Chromium's namespace sandbox creates its own user namespace. Probe the
// kernel capability once during preparation; the browser profile omits
// Bubblewrap's --disable-userns and permits the required clone/unshare flags.
fn verify_nested_user_namespace() -> Result<(), BoundaryError> {
    // SAFETY: the child performs only unshare(2) and _exit(2), both
    // async-signal-safe. It touches no allocator, lock or Rust runtime.
    let child = unsafe { libc::fork() };
    if child < 0 {
        return Err(browser_unavailable(
            "browser user-namespace capability probe could not start",
        ));
    }
    if child == 0 {
        let status = unsafe { libc::unshare(libc::CLONE_NEWUSER) };
        unsafe { libc::_exit(if status == 0 { 0 } else { 1 }) };
    }
    let mut status = 0;
    loop {
        // SAFETY: this is the unreaped child created immediately above.
        let waited = unsafe { libc::waitpid(child, &mut status, 0) };
        if waited == child {
            break;
        }
        if waited < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return Err(browser_unavailable(
            "browser user-namespace capability probe could not be reaped",
        ));
    }
    if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0 {
        Ok(())
    } else {
        Err(browser_unavailable(
            "nested user namespaces are unavailable; Chromium's own sandbox cannot be initialized",
        ))
    }
}

// Older kernels allowed connected pairs to regain a connectable/listening
// state after shutdown or a dead peer. Check behavior, including backports,
// in addition to the version gate. Finite probes cannot establish the absence
// of the historical dead-peer race by themselves.
fn verify_connected_pair_boundaries(scratch: &Path) -> io::Result<()> {
    let path = scratch.join("pair-probe");
    fs::DirBuilder::new().mode(0o700).create(&path)?;
    let directory = PairProbeDirectory(path);
    // The trusted host keeps this directory open throughout the probe. Its
    // descriptor path stays short even when the private TMPDIR exceeds sun_path.
    // No procfs path or directory descriptor is exposed to a sandbox payload.
    let directory_descriptor = fs::File::open(&directory.0)?;
    let address_root = PathBuf::from(format!(
        "/proc/self/fd/{}",
        directory_descriptor.as_raw_fd()
    ));
    let peer_close_deadline = Instant::now() + Duration::from_secs(1);
    for kind in [libc::SOCK_STREAM, libc::SOCK_SEQPACKET] {
        let listener = probe_socket(kind)?;
        let path = address_root.join(format!("{kind}.sock"));
        let address = probe_address(&path)?;
        let address_ptr = std::ptr::from_ref(&address).cast::<libc::sockaddr>();
        let length = std::mem::size_of_val(&address) as libc::socklen_t;
        // SAFETY: every descriptor and complete address is owned by this probe.
        check_probe_status(unsafe { libc::bind(listener.as_raw_fd(), address_ptr, length) })?;
        check_probe_status(unsafe { libc::listen(listener.as_raw_fd(), 1) })?;
        let control = probe_socket(kind)?;
        check_probe_status(unsafe { libc::connect(control.as_raw_fd(), address_ptr, length) })?;
        let accepted = unsafe {
            libc::accept4(
                listener.as_raw_fd(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            )
        };
        if accepted < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: accept4 returned a fresh descriptor, transferred exactly once.
        drop(unsafe { OwnedFd::from_raw_fd(accepted) });
        drop(control);
        for transition in [
            PairTransition::Unchanged,
            PairTransition::Unspecified,
            PairTransition::Shutdown(libc::SHUT_RD),
            PairTransition::Shutdown(libc::SHUT_WR),
            PairTransition::Shutdown(libc::SHUT_RDWR),
            PairTransition::PeerClosed,
            PairTransition::PeerClosedSend,
            PairTransition::PeerClosedSendTo,
            PairTransition::PeerClosedSendMsg,
        ] {
            let mut pair = [-1; 2];
            check_probe_status(unsafe {
                libc::socketpair(
                    libc::AF_UNIX,
                    kind | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                    0,
                    pair.as_mut_ptr(),
                )
            })?;
            // SAFETY: successful socketpair returned two independent descriptors.
            let left = unsafe { OwnedFd::from_raw_fd(pair[0]) };
            let right = unsafe { OwnedFd::from_raw_fd(pair[1]) };
            match transition {
                PairTransition::Unchanged => {}
                PairTransition::Unspecified => {
                    let address = libc::sockaddr {
                        sa_family: libc::AF_UNSPEC as _,
                        sa_data: [0; 14],
                    };
                    reject_probe_retarget(unsafe {
                        libc::connect(
                            left.as_raw_fd(),
                            &address,
                            std::mem::size_of_val(&address) as libc::socklen_t,
                        )
                    })?;
                }
                PairTransition::Shutdown(mode) => {
                    check_probe_status(unsafe { libc::shutdown(left.as_raw_fd(), mode) })?;
                }
                _ => {
                    drop(right);
                    wait_probe_peer_closed(left.as_raw_fd(), peer_close_deadline)?;
                    exercise_dead_peer(left.as_raw_fd(), transition)?;
                }
            }
            reject_probe_retarget(unsafe { libc::connect(left.as_raw_fd(), address_ptr, length) })?;
            let bound_path = directory.0.join(format!("{kind}-bound.sock"));
            let bound_address = probe_address(&address_root.join(format!("{kind}-bound.sock")))?;
            check_probe_status(unsafe {
                libc::bind(
                    left.as_raw_fd(),
                    std::ptr::from_ref(&bound_address).cast(),
                    length,
                )
            })?;
            reject_probe_retarget(unsafe { libc::listen(left.as_raw_fd(), 1) })?;
            fs::remove_file(bound_path)?;
        }
    }
    fs::remove_dir_all(&directory.0)?;
    Ok(())
}

struct PairProbeDirectory(PathBuf);
impl Drop for PairProbeDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn probe_address(path: &Path) -> io::Result<libc::sockaddr_un> {
    // SAFETY: zero initializes every field and padding byte of sockaddr_un.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as _;
    let bytes = path.as_os_str().as_bytes();
    if bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        return Err(io::Error::other(
            "kernel probe socket path exceeds its address bound",
        ));
    }
    for (target, byte) in address.sun_path.iter_mut().zip(bytes) {
        *target = *byte as _;
    }
    Ok(address)
}

#[derive(Clone, Copy)]
enum PairTransition {
    Unchanged,
    Unspecified,
    Shutdown(libc::c_int),
    PeerClosed,
    PeerClosedSend,
    PeerClosedSendTo,
    PeerClosedSendMsg,
}

fn probe_socket(kind: libc::c_int) -> io::Result<OwnedFd> {
    // SAFETY: socket does not dereference pointers and returns a new descriptor.
    let descriptor = unsafe {
        libc::socket(
            libc::AF_UNIX,
            kind | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: this is the sole owner of the descriptor returned by socket.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
}

fn check_probe_status(status: libc::c_int) -> io::Result<()> {
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn reject_probe_retarget(status: libc::c_int) -> io::Result<()> {
    if status == -1
        && matches!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::EISCONN | libc::EINVAL)
        )
    {
        return Ok(());
    }
    Err(io::Error::other(
        "anonymous pair could regain an external peer or listener",
    ))
}

fn wait_probe_peer_closed(descriptor: libc::c_int, deadline: Instant) -> io::Result<()> {
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "kernel probe peer references did not close before the probe deadline",
                )
            })?;
        let mut poll = libc::pollfd {
            fd: descriptor,
            events: 0,
            revents: 0,
        };
        let milliseconds = remaining.as_millis().clamp(1, i32::MAX as u128) as i32;
        // SAFETY: the probe owns this socket, and poll receives one complete descriptor.
        let status = unsafe { libc::poll(&mut poll, 1, milliseconds) };
        if status < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if status == 0 {
            continue;
        }
        if poll.revents & libc::POLLNVAL != 0 {
            return Err(io::Error::other(
                "kernel probe peer-close descriptor is invalid",
            ));
        }
        if Instant::now() >= deadline {
            continue;
        }
        if poll.revents & libc::POLLHUP != 0 {
            return Ok(());
        }
        return Err(io::Error::other(format!(
            "kernel probe peer-close reported unexpected poll flags {}",
            poll.revents
        )));
    }
}

fn exercise_dead_peer(descriptor: libc::c_int, transition: PairTransition) -> io::Result<()> {
    if matches!(transition, PairTransition::PeerClosed) {
        return Ok(());
    }
    let data = b"SAYA_SYNTHETIC_PAIR_PROBE";
    let mut vector = libc::iovec {
        iov_base: data.as_ptr().cast_mut().cast(),
        iov_len: data.len(),
    };
    // SAFETY: a zero msghdr starts with no destination or ancillary pointers.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut vector;
    message.msg_iovlen = 1;
    for _ in 0..3 {
        // SAFETY: each send references only the live synthetic byte buffer.
        // NOSIGNAL prevents this host-side capability probe from raising SIGPIPE.
        let status = unsafe {
            match transition {
                PairTransition::PeerClosedSend => libc::send(
                    descriptor,
                    data.as_ptr().cast(),
                    data.len(),
                    libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
                ),
                PairTransition::PeerClosedSendTo => libc::sendto(
                    descriptor,
                    data.as_ptr().cast(),
                    data.len(),
                    libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
                    std::ptr::null(),
                    0,
                ),
                PairTransition::PeerClosedSendMsg => libc::sendmsg(
                    descriptor,
                    &message,
                    libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
                ),
                _ => return Err(io::Error::other("unknown dead-peer probe transition")),
            }
        };
        let errno = (status == -1)
            .then(|| io::Error::last_os_error().raw_os_error())
            .flatten();
        if status != -1
            || !matches!(
                errno,
                Some(libc::EPIPE | libc::ECONNRESET | libc::ECONNREFUSED | libc::ENOTCONN)
            )
        {
            return Err(io::Error::other(format!(
                "dead-peer send did not report a known closed state (status={status}, errno={errno:?})"
            )));
        }
    }
    Ok(())
}
fn bind(
    args: &mut Vec<OsString>,
    descriptors: &mut Vec<Arc<fs::File>>,
    kind: &str,
    source: &Path,
    destination: &Path,
) -> Result<(), BoundaryError> {
    let descriptor = Arc::new(fs::File::open(source).map_err(super::io_error)?);
    bind_descriptor(args, descriptors, kind, descriptor, destination);
    Ok(())
}
fn bind_descriptor(
    args: &mut Vec<OsString>,
    descriptors: &mut Vec<Arc<fs::File>>,
    kind: &str,
    descriptor: Arc<fs::File>,
    destination: &Path,
) {
    args.extend([
        kind.into(),
        descriptor.as_raw_fd().to_string().into(),
        destination.as_os_str().to_owned(),
    ]);
    descriptors.push(descriptor);
}
// Procfs exposes the same-UID bridge through mem/fd; writable cgroup control
// files permit migration and resource changes. Include bind aliases and roots
// above or below a mount, while allowing a read-only cgroup filesystem view.
fn reject_control_filesystem_roots(policy: &SandboxPolicy) -> Result<(), BoundaryError> {
    let mounts = read_mount_table()?;
    reject_control_mount_aliases(policy, &mounts)
}
fn read_mount_table() -> Result<String, BoundaryError> {
    use std::io::Read;
    let mut mounts = String::new();
    fs::File::open("/proc/self/mountinfo")
        .map_err(super::io_error)?
        .take(4 * 1024 * 1024 + 1)
        .read_to_string(&mut mounts)
        .map_err(super::io_error)?;
    if mounts.len() > 4 * 1024 * 1024 {
        return Err(unavailable(
            "host mount table exceeds sandbox validation budget",
        ));
    }
    Ok(mounts)
}
// Browser installation trees are mounted read-only. Reject any tree that
// overlaps procfs, device filesystems or cgroup controllers, including through
// bind aliases, so the browser view cannot widen its control boundary.
fn reject_browser_install_roots(profile: &BrowserSandboxProfile) -> Result<(), BoundaryError> {
    let roots = profile.install_roots.iter().collect::<Vec<_>>();
    for root in &roots {
        for control in [Path::new("/proc"), Path::new("/dev")] {
            if root.starts_with(control) || control.starts_with(root.as_path()) {
                return Err(failure(
                    "unsupported_fs_policy",
                    "browser installation roots cannot overlap control filesystems",
                ));
            }
        }
    }
    let mounts = read_mount_table()?;
    reject_mount_aliases(&roots, &mounts, "proc")?;
    for filesystem in ["cgroup", "cgroup2"] {
        reject_mount_aliases(&roots, &mounts, filesystem)?;
    }
    Ok(())
}
fn reject_control_mount_aliases(policy: &SandboxPolicy, mounts: &str) -> Result<(), BoundaryError> {
    let roots = policy
        .read_roots
        .iter()
        .chain(&policy.write_roots)
        .collect::<Vec<_>>();
    if roots.iter().any(|root| root.starts_with("/proc")) {
        return Err(failure(
            "unsupported_fs_policy",
            "procfs cannot be exposed to sandbox payloads",
        ));
    }
    reject_mount_aliases(&roots, mounts, "proc")?;
    let write_roots = policy.write_roots.iter().collect::<Vec<_>>();
    for filesystem in ["cgroup", "cgroup2"] {
        reject_mount_aliases(&write_roots, mounts, filesystem)?;
    }
    Ok(())
}
fn reject_mount_aliases(
    roots: &[&PathBuf],
    mounts: &str,
    filesystem: &str,
) -> Result<(), BoundaryError> {
    for line in mounts.lines() {
        let (before, after) = line
            .split_once(" - ")
            .ok_or_else(|| unavailable("host mount table is malformed"))?;
        if after.split_whitespace().next() != Some(filesystem) {
            continue;
        }
        let mount = before
            .split_whitespace()
            .nth(4)
            .ok_or_else(|| unavailable("host mount table is malformed"))?;
        let mount = PathBuf::from(
            mount
                .replace("\\040", " ")
                .replace("\\011", "\t")
                .replace("\\012", "\n")
                .replace("\\134", "\\"),
        );
        if roots
            .iter()
            .any(|root| mount.starts_with(root) || root.starts_with(&mount))
        {
            return Err(failure(
                "unsupported_fs_policy",
                format!("a configured root exposes the protected {filesystem} control filesystem"),
            ));
        }
    }
    Ok(())
}
pub(super) fn safe_socat_path(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    path.is_absolute()
        && path
            .as_os_str()
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'/' | b'.' | b'_' | b'-'))
}
fn trusted_program(candidates: &[&str]) -> Result<PathBuf, BoundaryError> {
    for candidate in candidates {
        if let Ok(path) = fs::canonicalize(candidate)
            && let Ok(metadata) = fs::metadata(&path)
            && metadata.is_file()
            && metadata.uid() == 0
            && metadata.mode() & 0o022 == 0
            && metadata.mode() & 0o111 != 0
        {
            return Ok(path);
        }
    }
    Err(unavailable(
        "required root-owned Bubblewrap/socat executable is unavailable",
    ))
}
fn supported_version(version: &str) -> bool {
    let Some(value) = version.split_whitespace().last() else {
        return false;
    };
    let numbers = value
        .split('.')
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>();
    numbers.is_ok_and(|parts| parts.len() == 3 && (parts[0], parts[1], parts[2]) >= (0, 13, 1))
}
