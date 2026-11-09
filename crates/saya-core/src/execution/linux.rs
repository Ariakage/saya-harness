//! Bubblewrap filesystem views with a base filter and payload-only socket filter.
use super::{NetworkPolicy, PreparedCommand, SandboxPolicy, failure, unavailable};
use crate::contract::BoundaryError;
use std::{
    ffi::OsString,
    fs,
    io::{self, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
};

pub(super) struct LinuxBackend {
    bwrap: PathBuf,
    socat: Option<PathBuf>,
    args: Vec<OsString>,
    filter_path: PathBuf,
    mount_fds: Vec<Arc<fs::File>>,
    helper_path: PathBuf,
}
impl LinuxBackend {
    pub(super) fn helper_path(&self) -> &Path {
        &self.helper_path
    }
    pub(super) fn prepare(policy: &SandboxPolicy, scratch: &Path) -> Result<Self, BoundaryError> {
        reject_process_filesystem_roots(policy)?;
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
        let filter_path = scratch.join("base.bpf");
        let mut filter_file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&filter_path)
            .map_err(super::io_error)?;
        for instruction in filter(false) {
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
            "--disable-userns",
            "--new-session",
            "--die-with-parent",
            "--cap-drop",
            "ALL",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        let mut mount_fds = Vec::new();
        for path in ["/usr", "/bin", "/sbin", "/lib", "/lib64"] {
            if Path::new(path).exists() {
                bind(
                    &mut args,
                    &mut mount_fds,
                    "--ro-bind-fd",
                    Path::new(path),
                    Path::new(path),
                )?;
            }
        }
        for path in &policy.read_roots {
            bind(&mut args, &mut mount_fds, "--ro-bind-fd", path, path)?;
        }
        for path in &policy.write_roots {
            bind(&mut args, &mut mount_fds, "--bind-fd", path, path)?;
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
        let empty = scratch.join("empty");
        fs::create_dir(&empty).map_err(super::io_error)?;
        let mask = scratch.join("denied");
        fs::write(&mask, b"").map_err(super::io_error)?;
        let mask_file = Arc::new(fs::File::open(&mask).map_err(super::io_error)?);
        fs::set_permissions(&mask, fs::Permissions::from_mode(0o000)).map_err(super::io_error)?;
        for path in &policy.protected_paths {
            let visible = policy
                .read_roots
                .iter()
                .chain(&policy.write_roots)
                .any(|root| path.starts_with(root))
                || ["/usr", "/bin", "/sbin", "/lib", "/lib64"]
                    .iter()
                    .any(|root| path.starts_with(root));
            if !visible {
                continue;
            }
            match fs::symlink_metadata(path) {
                Ok(metadata) => {
                    if metadata.is_dir() {
                        bind(&mut args, &mut mount_fds, "--ro-bind-fd", &empty, path)?;
                    } else {
                        bind_descriptor(
                            &mut args,
                            &mut mount_fds,
                            "--ro-bind-fd",
                            mask_file.clone(),
                            path,
                        );
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    if policy.write_roots.iter().any(|root| path.starts_with(root)) {
                        return Err(failure(
                            "unsupported_fs_policy",
                            "a missing protected path inside a writable root cannot be safely masked",
                        ));
                    }
                }
                Err(error) => return Err(super::io_error(error)),
            }
        }
        let socat = match &policy.network {
            NetworkPolicy::Deny => None,
            NetworkPolicy::Proxy(proxy) => {
                if proxy.endpoint.ip() != std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST) {
                    return Err(unavailable("Linux proxy bridges require IPv4 loopback"));
                }
                let path = proxy.unix_socket.as_deref().ok_or_else(|| {
                    unavailable("Linux proxy isolation requires a private Unix socket")
                })?;
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
                        "proxy bridge path must be a Unix socket",
                    ));
                }
                if policy.write_roots.iter().any(|root| path.starts_with(root)) {
                    return Err(failure(
                        "invalid_sandbox_policy",
                        "proxy bridge socket must be outside writable roots",
                    ));
                }
                args.extend([
                    OsString::from("--ro-bind"),
                    path.as_os_str().to_owned(),
                    path.as_os_str().to_owned(),
                ]);
                Some(trusted_program(&[
                    "/usr/bin/socat",
                    "/bin/socat",
                    "/usr/local/bin/socat",
                ])?)
            }
        };
        let backend = Self {
            bwrap,
            socat,
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
        if let NetworkPolicy::Proxy(proxy) = &command.environment.policy.network {
            let socket = proxy
                .unix_socket
                .as_ref()
                .ok_or_else(|| unavailable("Linux proxy socket is unavailable"))?;
            let socat = self
                .socat
                .as_ref()
                .ok_or_else(|| unavailable("socat is unavailable"))?;
            command.args.extend([
                OsString::from("--socat"),
                socat.clone().into_os_string(),
                OsString::from("--socket"),
                socket.clone().into_os_string(),
                OsString::from("--port"),
                proxy.endpoint.port().to_string().into(),
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
// A proc mount would expose the same-UID bridge and launcher through mem/fd
// even when ptrace/process_vm syscalls are blocked. Keep procfs out of the view,
// including host aliases nested below an explicitly bound root.
fn reject_process_filesystem_roots(policy: &SandboxPolicy) -> Result<(), BoundaryError> {
    use std::io::Read;
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
    reject_proc_mount_aliases(&roots, &mounts)
}
fn reject_proc_mount_aliases(roots: &[&PathBuf], mounts: &str) -> Result<(), BoundaryError> {
    for line in mounts.lines() {
        let (before, after) = line
            .split_once(" - ")
            .ok_or_else(|| unavailable("host mount table is malformed"))?;
        if after.split_whitespace().next() != Some("proc") {
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
                "a configured root contains a procfs mount",
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
fn filter(payload: bool) -> Vec<libc::sock_filter> {
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
    let blocked = [
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
    if payload {
        // Anonymous connected streams support runtime self-pipes. They cannot
        // be retargeted like Unix datagram pairs. Named AF_UNIX socket creation,
        // datagram pairs and sequenced-packet pairs remain denied.
        program.extend([
            instruction(EQUAL, 0, 11, libc::SYS_socketpair as u32),
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
            instruction(EQUAL, 1, 0, libc::SOCK_STREAM as u32),
            instruction(RETURN, 0, 0, DENY),
            instruction(LOAD, 0, 0, 32),
            instruction(EQUAL, 1, 0, 0),
            instruction(RETURN, 0, 0, DENY),
            instruction(RETURN, 0, 0, ALLOW),
        ]);
    }
    for number in blocked {
        program.extend([
            instruction(EQUAL, 0, 1, number as u32),
            instruction(RETURN, 0, 0, DENY),
        ]);
    }
    // glibc falls back to clone when clone3 reports ENOSYS. Its pointed-to
    // flags cannot be inspected by classic BPF.
    program.extend([
        instruction(EQUAL, 0, 1, libc::SYS_clone3 as u32),
        instruction(RETURN, 0, 0, 0x0005_0000 | libc::ENOSYS as u32),
    ]);
    let namespaces = (libc::CLONE_NEWUSER
        | libc::CLONE_NEWNS
        | libc::CLONE_NEWPID
        | libc::CLONE_NEWNET
        | libc::CLONE_NEWIPC
        | libc::CLONE_NEWUTS
        | libc::CLONE_NEWCGROUP) as u32;
    program.extend([
        instruction(EQUAL, 0, 3, libc::SYS_clone as u32),
        instruction(LOAD, 0, 0, 16),
        instruction(BITS, 0, 1, namespaces),
        instruction(RETURN, 0, 0, DENY),
        instruction(LOAD, 0, 0, 0),
    ]);
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
pub(super) struct Filter {
    program: Vec<libc::sock_filter>,
}
impl Filter {
    pub(super) fn prepare() -> Self {
        Self {
            program: filter(true),
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
    fn evaluate(program: &[libc::sock_filter], arch: u32, syscall: u32, argument: u32) -> u32 {
        evaluate_arguments(program, arch, syscall, [argument, 0, 0])
    }
    fn evaluate_arguments(
        program: &[libc::sock_filter],
        arch: u32,
        syscall: u32,
        arguments: [u32; 3],
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
        for kind in [libc::SOCK_DGRAM, libc::SOCK_SEQPACKET, libc::SOCK_RAW] {
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
        assert_eq!(
            evaluate_arguments(
                &payload,
                arch,
                libc::SYS_socketpair as u32,
                [libc::AF_UNIX as u32, libc::SOCK_STREAM as u32, 1]
            ),
            DENY
        );
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
            assert!(reject_proc_mount_aliases(&[&root], mounts).is_err());
        }
        let other = PathBuf::from("/mnt/proc-alias-other");
        assert!(reject_proc_mount_aliases(&[&other], mounts).is_ok());
    }
}
