//! Real PTYs isolate the process-global Crossterm reader and signal handler.
use crossterm::event::{Event, KeyCode, KeyModifiers};
use saya_tui::{input::TerminalInput, terminal::TerminalGuard};
use std::{
    fs::File,
    io::{self, Read, Write},
    mem::MaybeUninit,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn input_child_probe() -> io::Result<()> {
    let Ok(mode) = std::env::var("SAYA_INPUT_PROBE") else {
        return Ok(());
    };
    let report = PathBuf::from(std::env::var_os("SAYA_INPUT_REPORT").unwrap());
    // SAFETY: stdin remains open through this child probe.
    let original_flags = unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_GETFL) };
    assert!(original_flags >= 0);
    if mode.contains("stress") {
        let hook_report = report.with_extension("hook");
        std::panic::set_hook(Box::new(move |_| {
            assert!(!crossterm::terminal::is_raw_mode_enabled().unwrap());
            std::fs::write(&hook_report, b"restored").unwrap();
            // Keep the prior hook running while the parent injects input at
            // the former race boundary, before the reader owner unwinds.
            std::thread::sleep(Duration::from_millis(20));
        }));
    }
    let mut terminal = TerminalGuard::enter()?;
    let mut input = TerminalInput::new()?;
    std::fs::write(report.with_extension("ready"), b"ready")?;
    if mode == "resize" {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    match input.next().await.unwrap().unwrap() {
                        Event::Key(key)
                            if key.code == KeyCode::Char('d')
                                && key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            break;
                        }
                        Event::Resize(_, _) => {}
                        event => panic!("unexpected input before Ctrl+D: {event:?}"),
                    }
                }
            })
            .await
            .unwrap();
        });
    } else if matches!(mode.as_str(), "full" | "partial") || mode.contains("stress") {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !report.with_extension("filled").exists() {
            assert!(Instant::now() < deadline, "parent did not inject input");
            std::thread::sleep(Duration::from_millis(5));
        }
        if mode.contains("full") {
            // Give the reader time to fill its bounded queue and wait in send.
            std::thread::sleep(Duration::from_millis(150));
        }
    } else if mode == "restore" {
        terminal.restore()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        assert!(
            runtime
                .block_on(async {
                    tokio::time::timeout(Duration::from_millis(200), input.next()).await
                })
                .unwrap()
                .is_none()
        );
    } else {
        assert!(matches!(
            mode.as_str(),
            "idle" | "idle-delayed-release" | "panic"
        ));
    }
    let started = Instant::now();
    if mode.contains("panic") {
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                let _input = input;
                panic!("input probe panic");
            }))
            .is_err()
        );
    } else {
        drop(input);
    }
    let dropped_millis = started.elapsed().as_millis();
    // Discard deliberately unconsumed input before restoring canonical mode.
    // A full canonical line could otherwise swallow the release handshake.
    // SAFETY: standard input is the live slave descriptor owned by this probe.
    if unsafe { libc::tcflush(libc::STDIN_FILENO, libc::TCIFLUSH) } < 0 {
        return Err(io::Error::last_os_error());
    }
    terminal.restore()?;
    // SAFETY: stdin remains open through restoration.
    assert_eq!(
        unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_GETFL) },
        original_flags
    );
    // Darwin marks raw-to-canonical transitions PENDIN until the next read.
    // Complete that empty canonical read before publishing the snapshot so the
    // parent's exact comparison cannot race the later release read_line.
    settle_canonical_input(libc::STDIN_FILENO)?;
    // SAFETY: stdin remains open, and the helper must restore its file flags.
    assert_eq!(
        unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_GETFL) },
        original_flags
    );
    let temporary = report.with_extension("tmp");
    std::fs::write(
        &temporary,
        serde_json::to_vec(&serde_json::json!({"dropped_millis":dropped_millis}))?,
    )?;
    std::fs::rename(temporary, &report)?;
    if mode == "idle-delayed-release" {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !report.with_extension("filled").exists() {
            assert!(
                Instant::now() < deadline,
                "parent did not inspect restored flags"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    // Darwin revokes a controlling terminal after its leader exits. Keep this
    // process alive until the parent has inspected the restored termios.
    let mut release = String::new();
    std::io::stdin().read_line(&mut release)?;
    Ok(())
}

fn settle_canonical_input(fd: libc::c_int) -> io::Result<()> {
    // SAFETY: callers retain ownership of the live PTY descriptor throughout.
    let original_flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if original_flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: F_SETFL only changes file flags for the same live descriptor.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, original_flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut byte = 0_u8;
    // SAFETY: byte is writable for one byte, and O_NONBLOCK bounds this read.
    let count = unsafe { libc::read(fd, std::ptr::addr_of_mut!(byte).cast(), 1) };
    let read_error = (count < 0).then(io::Error::last_os_error);
    // Restore flags on every read outcome before interpreting or returning it.
    // SAFETY: the descriptor remains live and original_flags came from F_GETFL.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, original_flags) } < 0 {
        return Err(io::Error::last_os_error());
    }
    match read_error {
        Some(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(()),
        Some(error) => Err(error),
        None => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected an empty canonical PTY, read returned {count}"),
        )),
    }
}

fn attributes(file: &File) -> io::Result<libc::termios> {
    let mut value = MaybeUninit::uninit();
    // SAFETY: file owns a live descriptor and value has space for one termios.
    if unsafe { libc::tcgetattr(file.as_raw_fd(), value.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: tcgetattr initialized the output value on success.
    Ok(unsafe { value.assume_init() })
}

struct Probe {
    master: File,
    slave: File,
    child: Child,
    original: libc::termios,
    output: Vec<u8>,
}
impl Probe {
    fn start(mode: &str, report: &Path) -> io::Result<Self> {
        let mut master = -1;
        let mut slave = -1;
        let mut size = libc::winsize {
            ws_col: 110,
            ws_row: 32,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: openpty initializes distinct output descriptors and reads size.
        if unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::addr_of_mut!(size),
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful openpty returned descriptors owned by these files.
        let master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
            // SAFETY: F_SETFD only changes descriptor flags for these live FDs.
            if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        // SAFETY: F_SETFL changes only flags for this owned descriptor.
        if unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let original = attributes(&slave)?;
        let child_tty = slave.as_raw_fd();
        let mut command = Command::new(std::env::current_exe()?);
        let stdin = if mode == "partial" || mode.contains("separate") {
            let mut name: [libc::c_char; 1024] = [0; 1024];
            // SAFETY: the slave is live and name has room for a terminal path.
            if unsafe { libc::ttyname_r(slave.as_raw_fd(), name.as_mut_ptr(), name.len()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: successful ttyname_r terminated name with a NUL byte.
            let name = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) };
            File::options()
                .read(true)
                .write(true)
                .open(std::ffi::OsStr::from_bytes(name.to_bytes()))?
        } else {
            slave.try_clone()?
        };
        command
            .args(["--exact", "input_child_probe", "--nocapture"])
            .env("SAYA_INPUT_PROBE", mode)
            .env("SAYA_INPUT_REPORT", report)
            .env("TERM", "xterm-256color")
            .stdin(Stdio::from(stdin))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave.try_clone()?));
        // SAFETY: child-only setup uses session and terminal ioctl operations.
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() < 0 || libc::ioctl(child_tty, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(Self {
            master,
            slave,
            child: command.spawn()?,
            original,
            output: Vec::new(),
        })
    }
    fn resize(&self, columns: u16, rows: u16) -> io::Result<()> {
        let size = libc::winsize {
            ws_col: columns,
            ws_row: rows,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: the slave FD is live and size describes the complete winsize.
        if unsafe { libc::ioctl(self.slave.as_raw_fd(), libc::TIOCSWINSZ as _, &size) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    fn drain(&mut self) -> io::Result<()> {
        let mut bytes = [0; 8192];
        loop {
            match self.master.read(&mut bytes) {
                Ok(0) => return Ok(()),
                Ok(count) => self.output.extend_from_slice(&bytes[..count]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) if error.raw_os_error() == Some(libc::EIO) => return Ok(()),
                Err(error) => return Err(error),
            }
        }
    }
    fn until_file(&mut self, path: &Path) -> io::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !path.exists() {
            self.drain()?;
            if self.child.try_wait()?.is_some() || Instant::now() >= deadline {
                return Err(io::Error::other(format!(
                    "input probe stopped before {}: {}",
                    path.display(),
                    String::from_utf8_lossy(&self.output)
                )));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }
    fn finish(&mut self, report: &Path) -> io::Result<()> {
        self.until_file(report)?;
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(report)?)?;
        assert!(value["dropped_millis"].as_u64().unwrap() < 400);
        let restored = attributes(&self.slave)?;
        assert_eq!(restored.c_iflag, self.original.c_iflag);
        assert_eq!(restored.c_oflag, self.original.c_oflag);
        assert_eq!(restored.c_cflag, self.original.c_cflag);
        assert_eq!(
            restored.c_lflag, self.original.c_lflag,
            "restored local flags"
        );
        assert_eq!(restored.c_cc, self.original.c_cc);
        self.master.write_all(b"release\n")?;
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            self.drain()?;
            if let Some(status) = self.child.try_wait()? {
                assert!(status.success(), "{status}");
                return Ok(());
            }
            assert!(Instant::now() < deadline, "probe did not exit");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for Probe {
    fn drop(&mut self) {
        let _ = self.child.kill();
        // A killed PTY child can wait for its queued output to drain on Darwin.
        // SAFETY: the owned slave descriptor is valid for discarding test I/O.
        unsafe { libc::tcflush(self.slave.as_raw_fd(), libc::TCIOFLUSH) };
        let _ = self.drain();
        let _ = self.child.wait();
    }
}

#[test]
fn resize_preserves_the_first_control_key_without_a_second_wakeup() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    for attempt in 0..12 {
        let report = root.path().join(format!("report-{attempt}.json"));
        let mut probe = Probe::start("resize", &report)?;
        probe.until_file(&report.with_extension("ready"))?;
        probe.resize(48, 16)?;
        probe.resize(110, 32)?;
        probe.master.write_all(b"\x04")?;
        probe.finish(&report)?;
    }
    Ok(())
}

#[test]
fn dropping_a_reader_with_a_full_queue_releases_the_sender() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let report = root.path().join("report.json");
    let mut probe = Probe::start("full", &report)?;
    probe.until_file(&report.with_extension("ready"))?;
    probe.master.write_all(&[b'x'; 256])?;
    std::fs::write(report.with_extension("filled"), b"filled")?;
    probe.finish(&report)
}

#[test]
fn dropping_an_idle_reader_waits_only_for_the_poll_interval() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let report = root.path().join("report.json");
    let mut probe = Probe::start("idle", &report)?;
    probe.finish(&report)
}

#[test]
fn restored_flags_are_observable_before_the_child_reads_its_release() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let report = root.path().join("report.json");
    let mut probe = Probe::start("idle-delayed-release", &report)?;
    probe.until_file(&report)?;
    assert_eq!(
        attributes(&probe.slave)?.c_lflag,
        probe.original.c_lflag,
        "restored local flags before the release read"
    );
    std::fs::write(report.with_extension("filled"), b"inspected")?;
    probe.finish(&report)
}

#[test]
fn canonical_input_settling_preserves_incorrect_local_flags() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let report = root.path().join("report.json");
    let mut probe = Probe::start("idle-delayed-release", &report)?;
    probe.until_file(&report)?;
    let mut incorrect = attributes(&probe.slave)?;
    incorrect.c_lflag ^= libc::ECHO;
    // SAFETY: the owned slave is live and incorrect is a complete termios value.
    if unsafe { libc::tcsetattr(probe.slave.as_raw_fd(), libc::TCSANOW, &incorrect) } < 0 {
        return Err(io::Error::last_os_error());
    }
    settle_canonical_input(probe.slave.as_raw_fd())?;
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| probe.finish(&report)))
        .expect_err("incorrect local flags must fail the restoration comparison");
    let message = failure
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| failure.downcast_ref::<&str>().copied())
        .unwrap_or("");
    assert!(message.contains("restored local flags"), "{message}");
    Ok(())
}

#[test]
fn partial_input_on_a_separately_opened_stdin_does_not_block_shutdown() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let report = root.path().join("report.json");
    let mut probe = Probe::start("partial", &report)?;
    probe.until_file(&report.with_extension("ready"))?;
    probe.master.write_all(b"\x1b[")?;
    std::fs::write(report.with_extension("filled"), b"filled")?;
    probe.finish(&report)
}

#[test]
fn panic_restoration_does_not_reenable_nonblocking_stdin() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let report = root.path().join("report.json");
    let mut probe = Probe::start("panic", &report)?;
    probe.finish(&report)
}

#[test]
fn restoring_the_terminal_stops_and_closes_its_reader_before_owner_drop() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let report = root.path().join("report.json");
    let mut probe = Probe::start("restore", &report)?;
    probe.finish(&report)
}

#[test]
fn incomplete_input_and_full_queue_panics_stop_before_terminal_restoration() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    for mode in ["stress-panic", "stress-separate-panic", "stress-full-panic"] {
        for attempt in 0..12 {
            let report = root.path().join(format!("{mode}-{attempt}.json"));
            let mut probe = Probe::start(mode, &report)?;
            probe.until_file(&report.with_extension("ready"))?;
            if mode.contains("full") {
                probe.master.write_all(&[b'x'; 256])?;
            } else {
                probe.master.write_all(b"\x1b[")?;
            }
            std::fs::write(report.with_extension("filled"), b"filled")?;
            if !mode.contains("full") {
                for _ in 0..16 {
                    if report.with_extension("hook").exists() {
                        break;
                    }
                    match probe.master.write(b"\x1b[") {
                        Ok(_) => {}
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                        Err(error) => return Err(error),
                    }
                    probe.drain()?;
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            probe.finish(&report)?;
        }
    }
    Ok(())
}
