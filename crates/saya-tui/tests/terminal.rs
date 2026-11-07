//! Real terminal-state checks run in child processes, isolating the panic hook.
use saya_tui::terminal::TerminalGuard;
use std::{
    fs::File,
    io::{self, Read, Write},
    mem::MaybeUninit,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::process::CommandExt,
    },
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn terminal_child_probe() -> io::Result<()> {
    let Ok(mode) = std::env::var("SAYA_TERMINAL_PROBE") else {
        return Ok(());
    };
    if mode == "notty" {
        assert_eq!(
            TerminalGuard::enter().err().unwrap().kind(),
            io::ErrorKind::InvalidInput
        );
        return Ok(());
    }
    match mode.as_str() {
        "normal" => {
            let mut terminal = TerminalGuard::enter()?;
            let size = terminal.size()?;
            assert_eq!((size.width, size.height), (80, 24));
            terminal.draw(|frame| {
                frame.render_widget(
                    ratatui::widgets::Paragraph::new("terminal probe"),
                    frame.area(),
                );
            })?;
            terminal.restore()?;
            // A restored guard may still exist while a new guard takes ownership.
            let mut next = TerminalGuard::enter()?;
            drop(terminal);
            assert!(crossterm::terminal::is_raw_mode_enabled()?);
            next.draw(|_| {})?;
        }
        "error" => {
            fn failing() -> io::Result<()> {
                let _terminal = TerminalGuard::enter()?;
                Err(io::Error::other("probe error"))
            }
            assert!(failing().is_err());
        }
        "panic" => {
            // Preserve the previously installed hook and verify restoration
            // occurs before that hook writes its diagnostic.
            std::panic::set_hook(Box::new(|_| {
                assert!(!crossterm::terminal::is_raw_mode_enabled().unwrap());
                eprintln!("previous panic hook ran");
            }));
            let result = std::panic::catch_unwind(|| {
                let mut terminal = TerminalGuard::enter().unwrap();
                terminal.draw(|_| panic!("probe panic")).unwrap();
            });
            assert!(result.is_err());
        }
        "nested" => {
            let terminal = TerminalGuard::enter()?;
            assert_eq!(
                TerminalGuard::enter().err().unwrap().kind(),
                io::ErrorKind::AlreadyExists
            );
            assert!(crossterm::terminal::is_raw_mode_enabled()?);
            drop(terminal);
        }
        _ => panic!("unknown terminal probe"),
    }
    assert!(!crossterm::terminal::is_raw_mode_enabled()?);
    eprintln!("terminal restored");
    let mut release = String::new();
    std::io::stdin().read_line(&mut release)?;
    Ok(())
}

fn terminal_attributes(file: &File) -> io::Result<libc::termios> {
    let mut value = MaybeUninit::uninit();
    // SAFETY: the pointer is valid for one termios and file owns the descriptor.
    if unsafe { libc::tcgetattr(file.as_raw_fd(), value.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: tcgetattr initialized value on success.
    Ok(unsafe { value.assume_init() })
}

fn drain_terminal(master: &mut File, output: &mut Vec<u8>) -> io::Result<()> {
    let mut chunk = [0u8; 4096];
    loop {
        match master.read(&mut chunk) {
            Ok(0) => return Ok(()),
            Ok(count) => output.extend_from_slice(&chunk[..count]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) if error.raw_os_error() == Some(libc::EIO) => return Ok(()),
            Err(error) => return Err(error),
        }
    }
}

fn run_terminal_probe(mode: &str) -> io::Result<Vec<u8>> {
    let mut master = -1;
    let mut slave = -1;
    let mut size = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: openpty initializes the two output descriptors and reads size.
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
    // SAFETY: successful openpty returned distinct owned descriptors.
    let mut master = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    let original = terminal_attributes(&slave)
        .map_err(|error| io::Error::other(format!("original attributes: {error}")))?;
    // SAFETY: master owns the descriptor, and F_SETFL changes only its flags.
    let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    let child_tty = slave.as_raw_fd();
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--exact", "terminal_child_probe", "--nocapture"])
        .env("SAYA_TERMINAL_PROBE", mode)
        .env("TERM", "xterm-256color")
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave.try_clone()?));
    // SAFETY: this child-only setup uses async-signal-safe session/ioctl calls.
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 || libc::ioctl(child_tty, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|error| io::Error::other(format!("spawn probe: {error}")))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut output = Vec::new();
    let mut restored = None;
    let status = loop {
        drain_terminal(&mut master, &mut output)?;
        if restored.is_none()
            && output
                .windows(b"terminal restored".len())
                .any(|bytes| bytes == b"terminal restored")
        {
            // Darwin revokes slave descriptors when the controlling session ends.
            // Inspect restored state while the child waits for this release.
            restored = Some(terminal_attributes(&slave)?);
            master.write_all(b"release\n")?;
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() > deadline {
            child.kill()?;
            child.wait()?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "terminal probe timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    drain_terminal(&mut master, &mut output)?;
    assert!(
        status.success(),
        "probe {mode}: {}",
        String::from_utf8_lossy(&output)
    );
    let restored =
        restored.ok_or_else(|| io::Error::other("terminal probe did not report restoration"))?;
    assert_eq!(restored.c_iflag, original.c_iflag, "{mode} input flags");
    assert_eq!(restored.c_oflag, original.c_oflag, "{mode} output flags");
    assert_eq!(restored.c_cflag, original.c_cflag, "{mode} control flags");
    assert_eq!(restored.c_lflag, original.c_lflag, "{mode} local flags");
    assert_eq!(restored.c_cc, original.c_cc, "{mode} control characters");
    Ok(output)
}

#[test]
fn normal_error_panic_and_nested_entry_restore_real_terminal_state() -> io::Result<()> {
    for mode in ["normal", "error", "panic", "nested"] {
        let output = run_terminal_probe(mode)?;
        let text = String::from_utf8_lossy(&output);
        assert!(
            text.contains("\u{1b}[?1049h"),
            "{mode} entered alternate screen"
        );
        assert!(
            text.contains("\u{1b}[?1049l"),
            "{mode} left alternate screen"
        );
        assert!(text.contains("\u{1b}[?25h"), "{mode} restored cursor");
        assert!(
            text.contains("\u{1b}[?2004l"),
            "{mode} restored bracketed paste"
        );
        if mode == "panic" {
            assert!(text.contains("previous panic hook ran"));
        }
    }
    Ok(())
}

#[test]
fn non_terminal_entry_has_no_terminal_side_effects() -> io::Result<()> {
    let output = Command::new(std::env::current_exe()?)
        .args(["--exact", "terminal_child_probe", "--nocapture"])
        .env("SAYA_TERMINAL_PROBE", "notty")
        .stdin(Stdio::null())
        .output()?;
    assert!(output.status.success());
    assert!(!output.stdout.contains(&0x1b));
    assert!(!output.stderr.contains(&0x1b));
    Ok(())
}
