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
        if matches!(policy.network, NetworkPolicy::LoopbackPort { .. }) {
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
pub(super) fn inherit_filter_descriptor(file: &fs::File) -> io::Result<()> {
    // SAFETY: fd is valid and these fcntl/prctl operations do not dereference pointers.
    unsafe {
        let flags = libc::fcntl(file.as_raw_fd(), libc::F_GETFD);
        if flags < 0 || libc::fcntl(file.as_raw_fd(), libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
        {
            return Err(io::Error::last_os_error());
        }
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}
fn instruction(code: u16, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}
const LOAD: u16 = 0x20;
const EQUAL: u16 = 0x15;
const BITS: u16 = 0x45;
const RETURN: u16 = 0x06;
const AND: u16 = 0x54;
const DENY: u32 = 0x0005_0000 | libc::EPERM as u32;
const ALLOW: u32 = 0x7fff_0000;
#[derive(Clone, Copy, PartialEq, Eq)]
enum FilterProfile {
    General,
    Browser,
}
const GENERAL_BLOCKED: &[libc::c_long] = &[
    libc::SYS_ptrace,
    libc::SYS_process_vm_readv,
    libc::SYS_process_vm_writev,
    libc::SYS_pidfd_getfd,
    libc::SYS_open_by_handle_at,
    libc::SYS_name_to_handle_at,
    libc::SYS_bpf,
    libc::SYS_perf_event_open,
    libc::SYS_keyctl,
    libc::SYS_add_key,
    libc::SYS_request_key,
    libc::SYS_mount,
    libc::SYS_umount2,
    libc::SYS_pivot_root,
    libc::SYS_move_mount,
    libc::SYS_open_tree,
    libc::SYS_fsopen,
    libc::SYS_fsconfig,
    libc::SYS_fsmount,
    libc::SYS_fspick,
    libc::SYS_mount_setattr,
    libc::SYS_setns,
    libc::SYS_unshare,
    libc::SYS_reboot,
    libc::SYS_kexec_load,
    libc::SYS_kexec_file_load,
    libc::SYS_init_module,
    libc::SYS_finit_module,
    libc::SYS_delete_module,
    libc::SYS_swapon,
    libc::SYS_swapoff,
    libc::SYS_syslog,
    libc::SYS_fanotify_init,
    libc::SYS_userfaultfd,
    libc::SYS_io_uring_setup,
    libc::SYS_io_uring_enter,
    libc::SYS_io_uring_register,
];
// Browser workers may create nested namespaces and mount inside their own
// namespace, so mount/umount/pivot_root/unshare are handled with argument
// checks below instead of being denied outright. All other generic denials
// remain, in particular setns, ptrace, pidfd_getfd and the new mount API.
const BROWSER_BLOCKED: &[libc::c_long] = &[
    libc::SYS_ptrace,
    libc::SYS_process_vm_readv,
    libc::SYS_process_vm_writev,
    libc::SYS_pidfd_getfd,
    libc::SYS_open_by_handle_at,
    libc::SYS_name_to_handle_at,
    libc::SYS_bpf,
    libc::SYS_perf_event_open,
    libc::SYS_keyctl,
    libc::SYS_add_key,
    libc::SYS_request_key,
    libc::SYS_move_mount,
    libc::SYS_open_tree,
    libc::SYS_fsopen,
    libc::SYS_fsconfig,
    libc::SYS_fsmount,
    libc::SYS_fspick,
    libc::SYS_mount_setattr,
    libc::SYS_setns,
    libc::SYS_reboot,
    libc::SYS_kexec_load,
    libc::SYS_kexec_file_load,
    libc::SYS_init_module,
    libc::SYS_finit_module,
    libc::SYS_delete_module,
    libc::SYS_swapon,
    libc::SYS_swapoff,
    libc::SYS_syslog,
    libc::SYS_fanotify_init,
    libc::SYS_userfaultfd,
    libc::SYS_io_uring_setup,
    libc::SYS_io_uring_enter,
    libc::SYS_io_uring_register,
];
fn filter(payload: bool) -> Vec<libc::sock_filter> {
    filter_for(payload, FilterProfile::General)
}
// Only selected by the trusted launcher when the host prepared a browser
// environment. The generic payload filter never widens.
fn browser_filter(payload: bool) -> Vec<libc::sock_filter> {
    filter_for(payload, FilterProfile::Browser)
}
fn filter_for(payload: bool, profile: FilterProfile) -> Vec<libc::sock_filter> {
    #[cfg(target_arch = "x86_64")]
    let arch = 0xc000_003e;
    #[cfg(target_arch = "aarch64")]
    let arch = 0xc000_00b7;
    let mut program = vec![
        instruction(LOAD, 0, 0, 4),
        instruction(EQUAL, 1, 0, arch),
        instruction(RETURN, 0, 0, 0x8000_0000),
        instruction(LOAD, 0, 0, 0),
    ];
    #[cfg(target_arch = "x86_64")]
    program.extend([
        instruction(BITS, 0, 1, 0x4000_0000),
        instruction(RETURN, 0, 0, 0x8000_0000),
    ]);
    if payload {
        // Anonymous connected streams and sequenced packets support runtime
        // self-pipes and Rust's child-exec error channel. Both keep a fixed peer,
        // unlike retargetable datagram pairs. Named AF_UNIX sockets stay denied.
        program.extend([
            instruction(EQUAL, 0, 12, libc::SYS_socketpair as u32),
            instruction(LOAD, 0, 0, 16),
            instruction(EQUAL, 1, 0, libc::AF_UNIX as u32),
            instruction(RETURN, 0, 0, DENY),
            instruction(LOAD, 0, 0, 24),
            instruction(
                AND,
                0,
                0,
                !(libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK) as u32,
            ),
            instruction(EQUAL, 2, 0, libc::SOCK_STREAM as u32),
            instruction(EQUAL, 1, 0, libc::SOCK_SEQPACKET as u32),
            instruction(RETURN, 0, 0, DENY),
            instruction(LOAD, 0, 0, 32),
            instruction(EQUAL, 1, 0, 0),
            instruction(RETURN, 0, 0, DENY),
            instruction(RETURN, 0, 0, ALLOW),
        ]);
    }
    let blocked = match profile {
        FilterProfile::General => GENERAL_BLOCKED,
        FilterProfile::Browser => BROWSER_BLOCKED,
    };
    for number in blocked {
        program.extend([
            instruction(EQUAL, 0, 1, *number as u32),
            instruction(RETURN, 0, 0, DENY),
        ]);
    }
    match profile {
        FilterProfile::General => {
            // glibc falls back to clone when clone3 reports ENOSYS. Its pointed-to
            // flags cannot be inspected by classic BPF.
            program.extend([
                instruction(EQUAL, 0, 1, libc::SYS_clone3 as u32),
                instruction(RETURN, 0, 0, 0x0005_0000 | libc::ENOSYS as u32),
            ]);
            let namespaces = browser_namespace_mask();
            program.extend([
                instruction(EQUAL, 0, 3, libc::SYS_clone as u32),
                instruction(LOAD, 0, 0, 16),
                instruction(BITS, 0, 1, namespaces),
                instruction(RETURN, 0, 0, DENY),
                instruction(LOAD, 0, 0, 0),
            ]);
        }
        FilterProfile::Browser => {
            // Classic BPF cannot inspect clone3's pointed-to flags. Accept only
            // the stable struct size and rely on the nested namespace plus the
            // blocked setns/new-mount API boundary; all other clone flags are
            // checked in the clone/unshare paths below.
            append_browser_clone3(&mut program);
            append_browser_flags(&mut program, libc::SYS_clone, 16, browser_clone_mask());
            append_browser_flags(&mut program, libc::SYS_unshare, 16, browser_unshare_mask());
            // mount(source, target, filesystemtype, mountflags, data): the
            // flags are arguments[3], which starts at offset 40 in seccomp_data.
            append_browser_flags(&mut program, libc::SYS_mount, 40, browser_mount_mask());
            append_browser_flags(&mut program, libc::SYS_umount2, 24, browser_umount_mask());
        }
    }
    let domains = if payload {
        vec![libc::AF_INET, libc::AF_INET6]
    } else {
        vec![libc::AF_INET, libc::AF_INET6, libc::AF_UNIX]
    };
    program.extend([
        instruction(EQUAL, 0, (domains.len() + 2) as u8, libc::SYS_socket as u32),
        instruction(LOAD, 0, 0, 16),
    ]);
    for (index, domain) in domains.iter().enumerate() {
        program.push(instruction(
            EQUAL,
            (domains.len() - index) as u8,
            0,
            *domain as u32,
        ));
    }
    program.extend([
        instruction(RETURN, 0, 0, DENY),
        instruction(RETURN, 0, 0, ALLOW),
    ]);
    program
}
// Check a syscall's flag argument against a permit mask. A matching syscall
// falls through to the argument checks; a nonmatching one skips the whole block.
fn append_browser_flags(
    program: &mut Vec<libc::sock_filter>,
    syscall: libc::c_long,
    offset: u32,
    allowed: u32,
) {
    program.extend([
        instruction(EQUAL, 0, 5, syscall as u32),
        instruction(LOAD, 0, 0, offset),
        instruction(AND, 0, 0, !allowed),
        instruction(EQUAL, 1, 0, 0),
        instruction(RETURN, 0, 0, DENY),
        instruction(RETURN, 0, 0, ALLOW),
    ]);
}
fn append_browser_clone3(program: &mut Vec<libc::sock_filter>) {
    // struct clone_args is eleven 64-bit fields on the supported architectures.
    const CLONE_ARGS_BYTES: u32 = (std::mem::size_of::<u64>() * 11) as u32;
    program.extend([
        instruction(EQUAL, 0, 4, libc::SYS_clone3 as u32),
        instruction(LOAD, 0, 0, 24),
        instruction(EQUAL, 1, 0, CLONE_ARGS_BYTES),
        instruction(RETURN, 0, 0, 0x0005_0000 | libc::ENOSYS as u32),
        instruction(RETURN, 0, 0, ALLOW),
    ]);
}
fn browser_namespace_mask() -> u32 {
    (libc::CLONE_NEWUSER
        | libc::CLONE_NEWNS
        | libc::CLONE_NEWPID
        | libc::CLONE_NEWNET
        | libc::CLONE_NEWIPC
        | libc::CLONE_NEWUTS
        | libc::CLONE_NEWCGROUP) as u32
}
fn browser_clone_mask() -> u32 {
    let ordinary = libc::CLONE_VM
        | libc::CLONE_FS
        | libc::CLONE_FILES
        | libc::CLONE_SIGHAND
        | libc::CLONE_PIDFD
        | libc::CLONE_VFORK
        | libc::CLONE_PARENT
        | libc::CLONE_THREAD
        | libc::CLONE_SYSVSEM
        | libc::CLONE_SETTLS
        | libc::CLONE_PARENT_SETTID
        | libc::CLONE_CHILD_CLEARTID
        | libc::CLONE_CHILD_SETTID
        | libc::CLONE_IO;
    // The low byte carries the exit signal. CLONE_PTRACE and unknown bits stay
    // denied.
    0xff | ordinary as u32 | browser_namespace_mask()
}
fn browser_unshare_mask() -> u32 {
    browser_namespace_mask()
        | (libc::CLONE_FS | libc::CLONE_FILES | libc::CLONE_SYSVSEM | libc::CLONE_IO) as u32
}
fn browser_mount_mask() -> u32 {
    (libc::MS_RDONLY
        | libc::MS_NOSUID
        | libc::MS_NODEV
        | libc::MS_NOEXEC
        | libc::MS_SYNCHRONOUS
        | libc::MS_REMOUNT
        | libc::MS_DIRSYNC
        | libc::MS_NOSYMFOLLOW
        | libc::MS_NOATIME
        | libc::MS_NODIRATIME
        | libc::MS_BIND
        | libc::MS_REC
        | libc::MS_SILENT
        | libc::MS_UNBINDABLE
        | libc::MS_PRIVATE
        | libc::MS_SLAVE
        | libc::MS_SHARED
        | libc::MS_RELATIME
        | libc::MS_I_VERSION
        | libc::MS_STRICTATIME
        | libc::MS_LAZYTIME) as u32
}
fn browser_umount_mask() -> u32 {
    (libc::MNT_FORCE | libc::MNT_DETACH | libc::MNT_EXPIRE | libc::UMOUNT_NOFOLLOW) as u32
}
pub(super) struct Filter {
    program: Vec<libc::sock_filter>,
}
impl Filter {
    pub(super) fn prepare() -> Self {
        // The trusted launcher receives this value from the host-configured
        // command environment, not from the payload environment.
        let browser = std::env::var_os("SAYA_BROWSER_SANDBOX")
            .is_some_and(|value| value.to_str() == Some("1"));
        Self {
            program: if browser {
                browser_filter(true)
            } else {
                filter(true)
            },
        }
    }
    pub(super) fn install(&self) -> io::Result<()> {
        let program = libc::sock_fprog {
            len: self.program.len() as u16,
            filter: self.program.as_ptr().cast_mut(),
        };
        // SAFETY: program owns the full initialized BPF array for both calls.
        unsafe {
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &program) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}
pub(super) fn install_payload_filter() -> io::Result<()> {
    Filter::prepare().install()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bridge_image_is_pinned_from_a_trusted_host_file_into_private_storage() {
        use std::os::unix::fs::symlink;

        let source =
            trusted_program(&["/usr/bin/true", "/bin/true"]).expect("root-owned fixture program");
        let scratch = tempfile::tempdir().expect("private bridge directory");
        let alias = scratch.path().join("source-alias");
        symlink(&source, &alias).expect("source alias");
        let original = fs::read(&source).expect("trusted source image");
        let (program, descriptor) = BridgeProgram::pin(&alias, scratch.path()).expect("pin bridge");
        let metadata = descriptor.metadata().expect("pinned image metadata");
        assert_eq!(program.path, scratch.path().join("saya-socat"));
        assert_eq!(
            (program.device, program.inode),
            (metadata.dev(), metadata.ino())
        );
        assert_eq!(metadata.mode() & 0o777, 0o500);
        assert_eq!(fs::read(&program.path).expect("private image"), original);
        fs::remove_file(&alias).expect("replace source alias");
        symlink("/nonexistent-replacement", &alias).expect("replacement alias");
        assert_eq!(
            fs::read(&program.path).expect("pinned image survives source replacement"),
            original
        );
    }
    #[test]
    fn bridge_image_rejects_an_untrusted_opened_host_program() {
        let scratch = tempfile::tempdir().expect("private bridge directory");
        let source = scratch.path().join("untrusted-program");
        fs::write(&source, b"untrusted").expect("fixture image");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o777)).expect("writable image");
        assert_eq!(
            BridgeProgram::pin(&source, scratch.path())
                .err()
                .expect("writable program must be rejected")
                .raw_os_error(),
            Some(libc::EPERM)
        );
        if unsafe { libc::geteuid() } != 0 {
            fs::set_permissions(&source, fs::Permissions::from_mode(0o500))
                .expect("readonly image");
            assert_eq!(
                BridgeProgram::pin(&source, scratch.path())
                    .err()
                    .expect("non-root host owner must be rejected")
                    .raw_os_error(),
                Some(libc::EPERM)
            );
        }
        assert!(!scratch.path().join("saya-socat").exists());
    }
    fn evaluate(program: &[libc::sock_filter], arch: u32, syscall: u32, argument: u32) -> u32 {
        evaluate_arguments(program, arch, syscall, [argument, 0, 0])
    }
    fn evaluate_arguments(
        program: &[libc::sock_filter],
        arch: u32,
        syscall: u32,
        arguments: [u32; 3],
    ) -> u32 {
        evaluate_arguments4(
            program,
            arch,
            syscall,
            [arguments[0], arguments[1], arguments[2], 0],
        )
    }
    fn evaluate_arguments4(
        program: &[libc::sock_filter],
        arch: u32,
        syscall: u32,
        arguments: [u32; 4],
    ) -> u32 {
        let mut accumulator = 0;
        let mut pc = 0;
        for _ in 0..program.len() {
            let instruction = &program[pc];
            match instruction.code {
                LOAD => {
                    accumulator = match instruction.k {
                        0 => syscall,
                        4 => arch,
                        16 => arguments[0],
                        24 => arguments[1],
                        32 => arguments[2],
                        40 => arguments[3],
                        _ => panic!("unexpected BPF load"),
                    }
                }
                EQUAL => {
                    pc += if accumulator == instruction.k {
                        instruction.jt as usize
                    } else {
                        instruction.jf as usize
                    }
                }
                BITS => {
                    pc += if accumulator & instruction.k != 0 {
                        instruction.jt as usize
                    } else {
                        instruction.jf as usize
                    }
                }
                AND => accumulator &= instruction.k,
                RETURN => return instruction.k,
                _ => panic!("unexpected BPF instruction"),
            }
            pc += 1;
            assert!(pc < program.len());
        }
        panic!("BPF failed to terminate");
    }
    #[test]
    fn base_and_payload_filters_enforce_abi_socket_and_namespace_boundaries() {
        #[cfg(target_arch = "x86_64")]
        let arch = 0xc000_003e;
        #[cfg(target_arch = "aarch64")]
        let arch = 0xc000_00b7;
        let base = filter(false);
        let payload = filter(true);
        assert_eq!(evaluate(&base, 0, libc::SYS_read as u32, 0), 0x8000_0000);
        for program in [&base, &payload] {
            assert_eq!(evaluate(program, arch, libc::SYS_read as u32, 0), ALLOW);
            assert_eq!(evaluate(program, arch, libc::SYS_ptrace as u32, 0), DENY);
            assert_eq!(evaluate(program, arch, libc::SYS_unshare as u32, 0), DENY);
            assert_eq!(
                evaluate(
                    program,
                    arch,
                    libc::SYS_clone as u32,
                    libc::CLONE_NEWUSER as u32
                ),
                DENY
            );
            assert_eq!(
                evaluate(program, arch, libc::SYS_clone as u32, libc::SIGCHLD as u32),
                ALLOW
            );
            assert_eq!(
                evaluate(program, arch, libc::SYS_clone3 as u32, 0),
                0x0005_0000 | libc::ENOSYS as u32
            );
            for domain in [libc::AF_INET, libc::AF_INET6] {
                assert_eq!(
                    evaluate(program, arch, libc::SYS_socket as u32, domain as u32),
                    ALLOW
                );
            }
            assert_eq!(
                evaluate(
                    program,
                    arch,
                    libc::SYS_socket as u32,
                    libc::AF_VSOCK as u32
                ),
                DENY
            );
        }
        assert_eq!(
            evaluate(
                &base,
                arch,
                libc::SYS_socketpair as u32,
                libc::AF_UNIX as u32
            ),
            ALLOW
        );
        assert_eq!(
            evaluate(
                &payload,
                arch,
                libc::SYS_socketpair as u32,
                libc::AF_UNIX as u32
            ),
            DENY
        );
        for kind in [
            libc::SOCK_STREAM,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            libc::SOCK_SEQPACKET,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
        ] {
            assert_eq!(
                evaluate_arguments(
                    &payload,
                    arch,
                    libc::SYS_socketpair as u32,
                    [libc::AF_UNIX as u32, kind as u32, 0]
                ),
                ALLOW
            );
        }
        for kind in [
            libc::SOCK_DGRAM,
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            libc::SOCK_RAW,
            libc::SOCK_STREAM | 0x1000,
            libc::SOCK_SEQPACKET | 0x1000,
        ] {
            assert_eq!(
                evaluate_arguments(
                    &payload,
                    arch,
                    libc::SYS_socketpair as u32,
                    [libc::AF_UNIX as u32, kind as u32, 0]
                ),
                DENY
            );
        }
        assert_eq!(
            evaluate_arguments(
                &payload,
                arch,
                libc::SYS_socketpair as u32,
                [libc::AF_INET as u32, libc::SOCK_STREAM as u32, 0]
            ),
            DENY
        );
        for kind in [libc::SOCK_STREAM, libc::SOCK_SEQPACKET] {
            assert_eq!(
                evaluate_arguments(
                    &payload,
                    arch,
                    libc::SYS_socketpair as u32,
                    [libc::AF_UNIX as u32, kind as u32, 1]
                ),
                DENY
            );
        }
        assert_eq!(
            evaluate(&base, arch, libc::SYS_socket as u32, libc::AF_UNIX as u32),
            ALLOW
        );
        assert_eq!(
            evaluate(
                &payload,
                arch,
                libc::SYS_socket as u32,
                libc::AF_UNIX as u32
            ),
            DENY
        );
        #[cfg(target_arch = "x86_64")]
        assert_eq!(
            evaluate(&payload, arch, 0x4000_0000 | libc::SYS_socket as u32, 0),
            0x8000_0000
        );
    }
    #[test]
    fn browser_filter_allows_required_namespaces_and_denies_widened_flags() {
        #[cfg(target_arch = "x86_64")]
        let arch = 0xc000_003e;
        #[cfg(target_arch = "aarch64")]
        let arch = 0xc000_00b7;
        let payload = browser_filter(true);
        let base = browser_filter(false);
        for program in [&payload, &base] {
            assert_eq!(
                evaluate_arguments(
                    program,
                    arch,
                    libc::SYS_clone as u32,
                    [libc::SIGCHLD as u32, 0, 0]
                ),
                ALLOW
            );
            assert_eq!(
                evaluate_arguments(
                    program,
                    arch,
                    libc::SYS_clone as u32,
                    [(libc::SIGCHLD | libc::CLONE_NEWUSER) as u32, 0, 0]
                ),
                ALLOW
            );
            assert_eq!(
                evaluate_arguments(
                    program,
                    arch,
                    libc::SYS_clone as u32,
                    [(libc::CLONE_NEWUSER | libc::CLONE_PTRACE) as u32, 0, 0]
                ),
                DENY
            );
            assert_eq!(
                evaluate_arguments(
                    program,
                    arch,
                    libc::SYS_unshare as u32,
                    [libc::CLONE_NEWUSER as u32, 0, 0]
                ),
                ALLOW
            );
            assert_eq!(
                evaluate_arguments(
                    program,
                    arch,
                    libc::SYS_unshare as u32,
                    [libc::CLONE_PTRACE as u32, 0, 0]
                ),
                DENY
            );
            assert_eq!(
                evaluate_arguments4(
                    program,
                    arch,
                    libc::SYS_mount as u32,
                    [
                        0,
                        0,
                        0,
                        (libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC) as u32
                    ]
                ),
                ALLOW
            );
            assert_eq!(
                evaluate_arguments4(
                    program,
                    arch,
                    libc::SYS_mount as u32,
                    [0, 0, 0, libc::MS_MOVE as u32]
                ),
                DENY
            );
            // A flags value placed in the filesystemtype slot (arguments[2],
            // offset 32) must not be mistaken for mount flags.
            assert_eq!(
                evaluate_arguments4(
                    program,
                    arch,
                    libc::SYS_mount as u32,
                    [
                        0,
                        0,
                        libc::MS_MOVE as u32,
                        (libc::MS_NOSUID | libc::MS_NODEV) as u32
                    ]
                ),
                ALLOW
            );
            assert_eq!(
                evaluate_arguments4(
                    program,
                    arch,
                    libc::SYS_mount as u32,
                    [0, 0, 0, (libc::MS_NOSUID | libc::MS_MOVE) as u32]
                ),
                DENY
            );
            assert_eq!(
                evaluate_arguments(
                    program,
                    arch,
                    libc::SYS_umount2 as u32,
                    [0, libc::MNT_DETACH as u32, 0]
                ),
                ALLOW
            );
            assert_eq!(
                evaluate_arguments(
                    program,
                    arch,
                    libc::SYS_umount2 as u32,
                    [0, libc::MS_MOVE as u32, 0]
                ),
                DENY
            );
            let clone_args_bytes = (std::mem::size_of::<u64>() * 11) as u32;
            assert_eq!(
                evaluate_arguments(
                    program,
                    arch,
                    libc::SYS_clone3 as u32,
                    [0, clone_args_bytes, 0]
                ),
                ALLOW
            );
            assert_eq!(
                evaluate_arguments(program, arch, libc::SYS_clone3 as u32, [0, 0, 0]),
                0x0005_0000 | libc::ENOSYS as u32
            );
            // Chromium sandbox needs these, but process-fd stealing and
            // namespace switching remain denied.
            for allowed in [
                libc::SYS_setresuid,
                libc::SYS_setresgid,
                libc::SYS_capset,
                libc::SYS_prctl,
                libc::SYS_seccomp,
                libc::SYS_memfd_create,
                libc::SYS_pidfd_open,
                libc::SYS_pidfd_send_signal,
                libc::SYS_chroot,
                libc::SYS_pivot_root,
            ] {
                assert_eq!(evaluate(program, arch, allowed as u32, 0), ALLOW);
            }
            for denied in [
                libc::SYS_ptrace,
                libc::SYS_pidfd_getfd,
                libc::SYS_setns,
                libc::SYS_move_mount,
                libc::SYS_mount_setattr,
                libc::SYS_open_tree,
                libc::SYS_bpf,
                libc::SYS_userfaultfd,
            ] {
                assert_eq!(evaluate(program, arch, denied as u32, 0), DENY);
            }
            assert_eq!(
                evaluate_arguments(
                    program,
                    arch,
                    libc::SYS_socketpair as u32,
                    [libc::AF_UNIX as u32, libc::SOCK_STREAM as u32, 0]
                ),
                ALLOW
            );
        }
        assert_eq!(
            evaluate_arguments(
                &payload,
                arch,
                libc::SYS_socketpair as u32,
                [libc::AF_UNIX as u32, libc::SOCK_DGRAM as u32, 0]
            ),
            DENY
        );
        assert_eq!(
            evaluate_arguments(
                &base,
                arch,
                libc::SYS_socketpair as u32,
                [libc::AF_UNIX as u32, libc::SOCK_DGRAM as u32, 0]
            ),
            ALLOW,
            "the trusted launcher may create the bridge socket pairs"
        );
        assert_eq!(
            evaluate(
                &payload,
                arch,
                libc::SYS_socket as u32,
                libc::AF_UNIX as u32
            ),
            DENY
        );
        assert_eq!(
            evaluate(&base, arch, libc::SYS_socket as u32, libc::AF_UNIX as u32),
            ALLOW
        );
        // The generic filter never widens to namespace or mount calls.
        let generic = filter(true);
        assert_eq!(evaluate(&generic, arch, libc::SYS_unshare as u32, 0), DENY);
        assert_eq!(evaluate(&generic, arch, libc::SYS_mount as u32, 0), DENY);
    }
    #[test]
    fn kernel_socket_pair_release_gate_accepts_only_confirmed_fixed_ranges() {
        for release in [
            "5.15.86",
            "5.15.200-generic",
            "6.1.2",
            "6.1.100-vendor",
            "6.2.0",
            "6.8.0-azure",
            "7.0.0+custom",
        ] {
            assert!(known_fixed_pair_kernel_release(release), "{release}");
        }
        for release in [
            "5.4.300",
            "5.10.250",
            "5.15.0-1092-azure",
            "5.15.85",
            "6.0.19",
            "6.1.1",
            "6.2.0-rc1",
            "6.2.0-RC1",
            "6.20.0-rc9",
            "6.2",
            "6.2.0.1",
            "6.2.0unknown",
            "unknown",
            "",
        ] {
            assert!(!known_fixed_pair_kernel_release(release), "{release}");
        }
    }
    #[test]
    fn connected_pair_kernel_probe_supports_long_private_paths_and_cleans_up() {
        let root = tempfile::tempdir().expect("probe scratch");
        verify_connected_pair_boundaries(root.path()).expect("connected pair kernel semantics");
        assert_eq!(
            fs::read_dir(root.path()).expect("scratch entries").count(),
            0
        );
        let long = root.path().join("x".repeat(120));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&long)
            .expect("long private probe scratch");
        assert!(long.as_os_str().as_bytes().len() > 108);
        verify_connected_pair_boundaries(&long)
            .expect("long scratch uses short host directory-fd addresses");
        assert_eq!(
            fs::read_dir(&long).expect("long scratch entries").count(),
            0
        );
    }

    fn kernel_probe_pair(kind: libc::c_int) -> [OwnedFd; 2] {
        let mut pair = [-1; 2];
        // SAFETY: socketpair initializes both descriptor slots on success.
        check_probe_status(unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                kind | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                0,
                pair.as_mut_ptr(),
            )
        })
        .expect("probe socket pair");
        // SAFETY: each successful socketpair descriptor has exactly one owner.
        pair.map(|descriptor| unsafe { OwnedFd::from_raw_fd(descriptor) })
    }

    struct ProbeFork(libc::pid_t);
    impl Drop for ProbeFork {
        fn drop(&mut self) {
            if self.0 > 0 {
                // SAFETY: this owned child remains unreaped, so its PID cannot be reused.
                unsafe {
                    libc::kill(self.0, libc::SIGKILL);
                    while libc::waitpid(self.0, std::ptr::null_mut(), 0) < 0
                        && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
                    {
                    }
                }
            }
        }
    }

    #[test]
    fn peer_close_probe_waits_for_inherited_cloexec_references_and_keeps_strict_sends() {
        for kind in [libc::SOCK_STREAM, libc::SOCK_SEQPACKET] {
            let [left, right] = kernel_probe_pair(kind);
            let [release_reader, release_writer] = kernel_probe_pair(libc::SOCK_STREAM);
            // SAFETY: the child performs no allocation or locking after fork.
            // Its bounded poll, descriptor operations and exec use only async-signal-safe syscalls.
            let pid = unsafe { libc::fork() };
            assert!(pid >= 0, "fork failed: {}", io::Error::last_os_error());
            if pid == 0 {
                unsafe {
                    libc::close(left.as_raw_fd());
                    libc::close(release_writer.as_raw_fd());
                    let mut poll = libc::pollfd {
                        fd: release_reader.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    if libc::poll(&mut poll, 1, 3000) != 1 {
                        libc::_exit(120);
                    }
                    let mut byte = 0u8;
                    if libc::read(
                        release_reader.as_raw_fd(),
                        std::ptr::from_mut(&mut byte).cast(),
                        1,
                    ) != 1
                    {
                        libc::_exit(121);
                    }
                    let arguments = [c"/usr/bin/true".as_ptr(), std::ptr::null()];
                    libc::execv(arguments[0], arguments.as_ptr());
                    libc::_exit(122);
                }
            }
            let mut child = ProbeFork(pid);
            drop(release_reader);
            drop(right);
            let held_deadline = Instant::now() + Duration::from_millis(20);
            assert_eq!(
                wait_probe_peer_closed(left.as_raw_fd(), held_deadline)
                    .expect_err("inherited peer must still be open")
                    .kind(),
                io::ErrorKind::TimedOut
            );
            let transitions = [
                PairTransition::PeerClosedSend,
                PairTransition::PeerClosedSendTo,
                PairTransition::PeerClosedSendMsg,
            ];
            for transition in transitions {
                let error = exercise_dead_peer(left.as_raw_fd(), transition)
                    .expect_err("a live inherited peer must not pass dead-peer send checks");
                assert!(error.to_string().contains("errno=None"), "{error}");
            }
            let byte = 1u8;
            // SAFETY: this owned control socket receives one synthetic byte.
            assert_eq!(
                unsafe {
                    libc::send(
                        release_writer.as_raw_fd(),
                        std::ptr::from_ref(&byte).cast(),
                        1,
                        libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
                    )
                },
                1
            );
            wait_probe_peer_closed(left.as_raw_fd(), Instant::now() + Duration::from_secs(1))
                .expect("exec must close the inherited CLOEXEC peer");
            for transition in transitions {
                exercise_dead_peer(left.as_raw_fd(), transition)
                    .expect("confirmed peer closure must preserve all strict send checks");
            }
            let mut status = 0;
            let waited = loop {
                // SAFETY: this is the unreaped child owned by the local guard.
                let waited = unsafe { libc::waitpid(pid, &mut status, 0) };
                if waited >= 0 || io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                    break waited;
                }
            };
            assert_eq!(waited, pid);
            child.0 = 0;
            assert_eq!(status, 0, "controlled child failed to exec true");
            assert_eq!(
                wait_probe_peer_closed(left.as_raw_fd(), held_deadline)
                    .expect_err("an expired shared deadline must remain expired")
                    .kind(),
                io::ErrorKind::TimedOut
            );
        }
    }

    #[test]
    fn peer_close_probe_accepts_closed_peers_without_inheritance() {
        for kind in [libc::SOCK_STREAM, libc::SOCK_SEQPACKET] {
            let [left, right] = kernel_probe_pair(kind);
            drop(right);
            wait_probe_peer_closed(left.as_raw_fd(), Instant::now() + Duration::from_secs(1))
                .expect("closed peer must produce a hangup");
            for transition in [
                PairTransition::PeerClosed,
                PairTransition::PeerClosedSend,
                PairTransition::PeerClosedSendTo,
                PairTransition::PeerClosedSendMsg,
            ] {
                exercise_dead_peer(left.as_raw_fd(), transition)
                    .expect("all closed-peer transitions must keep their strict result checks");
            }
        }
    }

    #[test]
    fn protection_masks_cover_canonical_system_aliases_and_parent_denies() {
        let mut policy = SandboxPolicy::workspace("/workspace".into());
        policy.read_roots.push("/usr".into());
        policy.protected_paths = vec![
            "/usr/lib/private/key".into(),
            "/usr/bin/env".into(),
            "/etc".into(),
        ];
        let roots = vec![
            ("/usr".into(), "/usr".into()),
            ("/usr/lib".into(), "/lib".into()),
            ("/usr/bin".into(), "/bin".into()),
            ("/etc/alternatives".into(), "/etc/alternatives".into()),
        ];
        let masks = protection_masks(&policy, &roots).unwrap();
        assert_eq!(
            masks,
            vec![
                ("/usr/bin/env".into(), "/bin/env".into()),
                ("/etc/alternatives".into(), "/etc/alternatives".into()),
                ("/usr/lib/private/key".into(), "/lib/private/key".into()),
                ("/usr/bin/env".into(), "/usr/bin/env".into()),
                ("/usr/lib/private/key".into(), "/usr/lib/private/key".into()),
            ]
        );
        policy.protected_paths = vec!["/usr/lib/private/key".into(), "/usr/lib".into()];
        assert_eq!(
            protection_masks(&policy, &roots).unwrap(),
            vec![
                ("/usr/lib".into(), "/lib".into()),
                ("/usr/lib".into(), "/usr/lib".into()),
            ]
        );
        policy.protected_paths = vec!["/workspace/private".into()];
        policy
            .read_roots
            .push("/workspace/private/subdirectory".into());
        assert_eq!(
            protection_masks(&policy, &roots).unwrap(),
            vec![("/workspace/private".into(), "/workspace/private".into()),]
        );
    }
    #[test]
    fn protection_mask_growth_is_bounded() {
        let mut policy = SandboxPolicy::workspace("/workspace".into());
        policy.protected_paths = (0..4097)
            .map(|index| PathBuf::from(format!("/workspace/{index}")))
            .collect();
        assert_eq!(
            protection_masks(&policy, &[]).unwrap_err().0.code,
            "unsupported_fs_policy"
        );
    }
    #[test]
    fn system_alternatives_resolve_read_only_and_stay_subject_to_protected_masks() {
        use std::os::unix::process::CommandExt;
        fn run_view(
            backend: &LinuxBackend,
            workspace: &Path,
            argv: &[&str],
        ) -> std::process::Output {
            let mut command = Command::new(&backend.bwrap);
            command
                .args(&backend.args)
                .args(argv)
                .current_dir(workspace)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("LC_ALL", "C")
                .stdin(Stdio::null());
            let descriptors = backend.mount_fds.clone();
            // SAFETY: the inherited descriptors remain owned; the callback only
            // changes their inheritance and no-new-privileges before exec.
            unsafe {
                command.pre_exec(move || {
                    for descriptor in &descriptors {
                        inherit_filter_descriptor(descriptor)?;
                    }
                    Ok(())
                });
            }
            command.output().expect("native system-root view")
        }
        let alternatives = Path::new("/etc/alternatives");
        if !alternatives.exists() {
            eprintln!(
                "system alternatives are absent on this distribution; alias fixture not applicable"
            );
            return;
        }
        let control = Command::new("/usr/bin/cc")
            .arg("--version")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LC_ALL", "C")
            .output()
            .expect("host compiler positive control");
        assert!(
            control.status.success(),
            "host compiler positive control failed"
        );
        let workspace = tempfile::tempdir().expect("workspace");
        let scratch = tempfile::tempdir().expect("native scratch");
        let mut policy = SandboxPolicy::workspace(workspace.path().canonicalize().unwrap());
        let backend = LinuxBackend::prepare(&policy, scratch.path()).expect("native view");
        let version = run_view(&backend, &policy.workspace, &["/usr/bin/cc", "--version"]);
        assert!(
            version.status.success(),
            "native cc alias: {}",
            String::from_utf8_lossy(&version.stderr)
        );
        assert_eq!(version.stdout, control.stdout);
        let check = run_view(
            &backend,
            &policy.workspace,
            &[
                "/usr/bin/python3",
                "-I",
                "-S",
                "-c",
                "import os; assert os.statvfs('/etc/alternatives').f_flag & os.ST_RDONLY; assert not any(os.path.exists(p) for p in ('/etc/passwd','/etc/shadow','/etc/hostname')); print('SAYA_ALTERNATIVES_READ_ONLY')",
            ],
        );
        assert!(
            check.status.success(),
            "native metadata boundary: {}",
            String::from_utf8_lossy(&check.stderr)
        );
        let protected_scratch = tempfile::tempdir().expect("protected native scratch");
        policy
            .protected_paths
            .push(alternatives.canonicalize().unwrap());
        let protected =
            LinuxBackend::prepare(&policy, protected_scratch.path()).expect("protected view");
        let hidden = run_view(
            &protected,
            &policy.workspace,
            &[
                "/usr/bin/python3",
                "-I",
                "-S",
                "-c",
                "import os; assert os.listdir('/etc/alternatives') == []; assert os.statvfs('/etc/alternatives').f_flag & os.ST_RDONLY",
            ],
        );
        assert!(
            hidden.status.success(),
            "system-root protection mask: {}",
            String::from_utf8_lossy(&hidden.stderr)
        );
        if fs::read_link("/usr/bin/cc").is_ok_and(|target| target.starts_with(alternatives)) {
            assert!(
                !run_view(&protected, &policy.workspace, &["/usr/bin/cc", "--version"])
                    .status
                    .success()
            );
        }
        let canonical = fs::canonicalize("/usr/bin/readlink").unwrap();
        let alias_matches = fs::canonicalize("/bin/readlink").is_ok_and(|alias| alias == canonical);
        if alias_matches {
            policy.protected_paths.push(canonical);
            let alias_scratch = tempfile::tempdir().expect("system alias scratch");
            let alias =
                LinuxBackend::prepare(&policy, alias_scratch.path()).expect("alias-protected view");
            let denied = run_view(
                &alias,
                &policy.workspace,
                &[
                    "/usr/bin/python3",
                    "-I",
                    "-S",
                    "-c",
                    "import os\nfor path in ('/usr/bin/readlink','/bin/readlink'):\n try:\n  fd=os.open(path,os.O_RDONLY)\n except PermissionError:\n  continue\n os.close(fd)\n raise AssertionError('protected system alias was readable')",
                ],
            );
            assert!(
                denied.status.success(),
                "canonical/guest alias protection: {}",
                String::from_utf8_lossy(&denied.stderr)
            );
        }
        if std::env::var_os("GITHUB_ACTIONS").is_some() {
            let evidence = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/n2/acceptance/system-alternatives.json");
            fs::create_dir_all(evidence.parent().unwrap()).expect("evidence directory");
            let observation = serde_json::json!({
                "cc_link": fs::read_link("/usr/bin/cc").ok(),
                "alternatives_cc_link": fs::read_link("/etc/alternatives/cc").ok(),
                "cc_resolved": fs::canonicalize("/usr/bin/cc").unwrap(),
                "native_cc_matches_host": true,
                "alternatives_read_only": true,
                "other_etc_files_absent": true,
                "whole_alternatives_protection_mask": true,
                "canonical_and_guest_alias_protection": alias_matches,
            });
            fs::write(evidence, serde_json::to_vec_pretty(&observation).unwrap())
                .expect("system alias evidence");
        }
    }
    #[test]
    fn supported_bubblewrap_version_requires_the_validated_security_baseline() {
        assert!(supported_version("bubblewrap 0.13.1\n"));
        assert!(supported_version("bubblewrap 0.14.0"));
        assert!(!supported_version("bubblewrap 0.6.1"));
        assert!(!supported_version("bubblewrap 0.13.0"));
        assert!(!supported_version("bubblewrap unknown"));
    }
    #[test]
    fn proc_aliases_and_their_descendants_are_not_bound_into_the_payload() {
        let mounts = "1 0 0:1 / / rw - ext4 root rw\n2 1 0:2 / /mnt/proc-alias rw - proc proc rw\n";
        for root in ["/mnt", "/mnt/proc-alias", "/mnt/proc-alias/123/fd"] {
            let root = PathBuf::from(root);
            assert!(reject_mount_aliases(&[&root], mounts, "proc").is_err());
        }
        let other = PathBuf::from("/mnt/proc-alias-other");
        assert!(reject_mount_aliases(&[&other], mounts, "proc").is_ok());
    }
    #[test]
    fn cgroup_filesystems_are_read_only_even_through_bind_aliases() {
        let mounts = "1 0 0:1 / / rw - ext4 root rw\n\
            2 1 0:2 / /sys/fs/cgroup rw - cgroup2 cgroup rw\n\
            3 1 0:2 /delegated /mnt/cgroup-alias rw - cgroup2 cgroup rw\n\
            4 1 0:3 /cpu /mnt/legacy-alias rw - cgroup cpu rw\n\
            5 1 0:2 /delegated /escaped\\040control/cgroup rw - cgroup2 cgroup rw\n";
        let mut policy = SandboxPolicy::workspace("/workspace".into());
        policy.read_roots.extend([
            "/sys/fs/cgroup".into(),
            "/mnt/cgroup-alias/child".into(),
            "/mnt/legacy-alias".into(),
            "/escaped control/cgroup".into(),
        ]);
        reject_control_mount_aliases(&policy, mounts).expect("read-only controller views");
        for root in [
            "/sys",
            "/sys/fs/cgroup",
            "/sys/fs/cgroup/other-group",
            "/mnt",
            "/mnt/cgroup-alias",
            "/mnt/cgroup-alias/child",
            "/mnt/legacy-alias",
            "/mnt/legacy-alias/child",
            "/escaped control",
            "/escaped control/cgroup/child",
        ] {
            policy.write_roots = vec![root.into()];
            let error = reject_control_mount_aliases(&policy, mounts).expect_err(root);
            assert_eq!(error.0.code, "unsupported_fs_policy", "{root}");
        }
        for root in [
            "/workspace",
            "/sys/fs/cgroup-neighbor",
            "/mnt/cgroup-alias-other",
        ] {
            policy.write_roots = vec![root.into()];
            reject_control_mount_aliases(&policy, mounts).expect(root);
        }
    }
    #[test]
    fn procfs_remains_denied_for_read_only_roots_after_control_mount_extension() {
        let mounts = "1 0 0:1 / / rw - ext4 root rw\n2 1 0:2 / /mnt/proc-alias rw - proc proc rw\n";
        let mut policy = SandboxPolicy::workspace("/workspace".into());
        for root in ["/proc", "/proc/self/fd", "/mnt", "/mnt/proc-alias/123/fd"] {
            policy.read_roots = vec![root.into()];
            assert!(
                reject_control_mount_aliases(&policy, mounts).is_err(),
                "{root}"
            );
        }
        policy.read_roots = vec!["/mnt/proc-alias-neighbor".into()];
        reject_control_mount_aliases(&policy, mounts).expect("nonoverlapping read root");
    }
}
