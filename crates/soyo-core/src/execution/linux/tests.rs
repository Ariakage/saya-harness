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
        fs::set_permissions(&source, fs::Permissions::from_mode(0o500)).expect("readonly image");
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
fn write_deny_masks_cover_writable_roots_and_system_aliases() {
    let mut policy = SandboxPolicy::workspace("/workspace".into());
    policy.read_roots.push("/usr".into());
    policy.write_denied_paths = vec![
        "/workspace/.git/config".into(),
        "/workspace/.git/hooks".into(),
        "/outside/.git/config".into(),
    ];
    let roots = vec![
        ("/usr".into(), "/usr".into()),
        ("/workspace".into(), "/w".into()),
    ];
    assert_eq!(
        write_deny_masks(&policy, &roots).unwrap(),
        vec![
            ("/workspace/.git/config".into(), "/w/.git/config".into()),
            ("/workspace/.git/hooks".into(), "/w/.git/hooks".into()),
            (
                "/workspace/.git/config".into(),
                "/workspace/.git/config".into()
            ),
            (
                "/workspace/.git/hooks".into(),
                "/workspace/.git/hooks".into()
            ),
        ]
    );
}
#[test]
fn write_deny_mask_growth_is_bounded() {
    let mut policy = SandboxPolicy::workspace("/workspace".into());
    policy.write_denied_paths = (0..4097)
        .map(|index| PathBuf::from(format!("/workspace/{index}")))
        .collect();
    assert!(write_deny_masks(&policy, &[]).is_err());
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
    fn run_view(backend: &LinuxBackend, workspace: &Path, argv: &[&str]) -> std::process::Output {
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
            "import os; assert os.statvfs('/etc/alternatives').f_flag & os.ST_RDONLY; assert not any(os.path.exists(p) for p in ('/etc/passwd','/etc/shadow','/etc/hostname')); print('SOYO_ALTERNATIVES_READ_ONLY')",
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
