use std::{fs, io, os::fd::AsRawFd};

pub(in crate::execution) fn inherit_filter_descriptor(file: &fs::File) -> io::Result<()> {
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
pub(super) fn instruction(code: u16, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}
pub(super) const LOAD: u16 = 0x20;
pub(super) const EQUAL: u16 = 0x15;
pub(super) const BITS: u16 = 0x45;
pub(super) const RETURN: u16 = 0x06;
pub(super) const AND: u16 = 0x54;
pub(super) const DENY: u32 = 0x0005_0000 | libc::EPERM as u32;
pub(super) const ALLOW: u32 = 0x7fff_0000;
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
pub(super) fn filter(payload: bool) -> Vec<libc::sock_filter> {
    filter_for(payload, FilterProfile::General)
}
// Only selected by the trusted launcher when the host prepared a browser
// environment. The generic payload filter never widens.
pub(super) fn browser_filter(payload: bool) -> Vec<libc::sock_filter> {
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
pub(in crate::execution) fn install_payload_filter() -> io::Result<()> {
    Filter::prepare().install()
}
