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
    path::Path,
    process::{Child, Command, Stdio},
    sync::Mutex,
    time::{Duration, Instant},
};

// openpty cannot set CLOEXEC atomically. Keep parallel test threads from
// spawning while another fixture is between openpty and its fcntl calls.
static PTY_TESTS: Mutex<()> = Mutex::new(());

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
    if matches!(mode.as_str(), "slow" | "slow-panic" | "closed" | "drip") {
        return output_fault_child(&mode);
    }
    let original_flags = fd_flags(libc::STDOUT_FILENO)?;
    match mode.as_str() {
        "normal" => {
            let mut terminal = TerminalGuard::enter()?;
            terminal.set_title("Saya | normal 🌸")?;
            terminal.set_title("Saya | normal 🌸")?;
            terminal.set_title(" Saya   | normal 🌸\x07")?;
            let size = terminal.size()?;
            assert_eq!((size.width, size.height), (80, 24));
            terminal.draw(|frame| {
                frame.render_widget(
                    ratatui::widgets::Paragraph::new("terminal probe"),
                    frame.area(),
                );
            })?;
            terminal.restore()?;
            assert_eq!(
                terminal.set_title("ended").unwrap_err().kind(),
                io::ErrorKind::BrokenPipe
            );
            assert_eq!(
                terminal
                    .draw(|_| panic!("restored guard rendered"))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::BrokenPipe
            );
            // A restored guard may still exist while a new guard takes ownership.
            let mut next = TerminalGuard::enter()?;
            drop(terminal);
            next.set_title("Saya | next")?;
            assert!(crossterm::terminal::is_raw_mode_enabled()?);
            next.draw(|_| {})?;
        }
        "error" => {
            fn failing() -> io::Result<()> {
                let mut terminal = TerminalGuard::enter()?;
                terminal.set_title("Saya | error")?;
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
                terminal.set_title("Saya | panic").unwrap();
                terminal.draw(|_| panic!("probe panic")).unwrap();
            });
            assert!(result.is_err());
        }
        "nested" => {
            let mut terminal = TerminalGuard::enter()?;
            terminal.set_title("Saya | nested")?;
            assert_eq!(
                TerminalGuard::enter().err().unwrap().kind(),
                io::ErrorKind::AlreadyExists
            );
            assert!(crossterm::terminal::is_raw_mode_enabled()?);
            drop(terminal);
        }
        "render-delay" => {
            let mut terminal = TerminalGuard::enter()?;
            let mut writable = false;
            let result = terminal.draw(|frame| {
                std::thread::sleep(Duration::from_millis(300));
                let mut descriptor = libc::pollfd {
                    fd: libc::STDOUT_FILENO,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                // SAFETY: stdout is the live PTY and poll receives one valid descriptor.
                writable = unsafe { libc::poll(&mut descriptor, 1, 0) } > 0
                    && descriptor.revents & libc::POLLOUT != 0;
                frame.render_widget("WRITABLE_AFTER_SLOW_RENDER", frame.area());
            });
            assert!(writable, "fixture PTY was not writable after rendering");
            result?;
        }
        "titles" => {
            let mut terminal = TerminalGuard::enter()?;
            let malicious = "Saya | safe\x1b]52;c;payload\x07\x1b\\\r\nnext\u{009d}8;;link\u{009c}\u{202e}RTL\u{2066}end\u{2069}";
            terminal.set_title(malicious)?;
            terminal.set_title(malicious)?;
            let controls: String = (0..=31)
                .chain(127..=159)
                .filter_map(char::from_u32)
                .collect();
            terminal.set_title(&format!("A{controls}B"))?;
            let large = format!("Saya | {}END", "🌸界".repeat(1000));
            terminal.set_title(&large)?;
            terminal.set_title(&large)?;
            terminal.draw(|frame| {
                frame.render_widget(
                    ratatui::widgets::Paragraph::new("AFTER_TITLE"),
                    frame.area(),
                )
            })?;
        }
        _ => panic!("unknown terminal probe"),
    }
    assert!(!crossterm::terminal::is_raw_mode_enabled()?);
    if let Some(path) = std::env::var_os("SAYA_TERMINAL_REPORT") {
        report(
            Path::new(&path),
            serde_json::json!({"original_flags":original_flags, "restored_flags":fd_flags(libc::STDOUT_FILENO)?}),
        )?;
    }
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

struct PtyProbe {
    master: File,
    slave: File,
    child: Child,
    original: libc::termios,
}
fn spawn_probe(mode: &str, report: Option<&Path>) -> io::Result<PtyProbe> {
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
    let master = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
        // SAFETY: owned PTY descriptors remain live through child setup.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
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
    if let Some(report) = report {
        command.env("SAYA_TERMINAL_REPORT", report);
    }
    // SAFETY: this child-only setup uses async-signal-safe session/ioctl calls.
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 || libc::ioctl(child_tty, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command
        .spawn()
        .map_err(|error| io::Error::other(format!("spawn probe: {error}")))?;
    Ok(PtyProbe {
        master,
        slave,
        child,
        original,
    })
}

fn run_terminal_probe(mode: &str) -> io::Result<Vec<u8>> {
    let report_root = tempfile::tempdir()?;
    let report_path = report_root.path().join("report.json");
    let PtyProbe {
        mut master,
        slave,
        mut child,
        original,
    } = spawn_probe(mode, Some(&report_path))?;
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
            let flags: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&report_path)?).map_err(io::Error::other)?;
            assert_eq!(flags["restored_flags"], flags["original_flags"]);
            assert_eq!(
                fd_flags(slave.as_raw_fd())?,
                flags["original_flags"].as_i64().unwrap() as i32,
                "{mode} descriptor flags"
            );
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
    let _serial = PTY_TESTS.lock().unwrap();
    for mode in ["normal", "error", "panic", "nested"] {
        let output = run_terminal_probe(mode)?;
        let text = String::from_utf8_lossy(&output);
        assert!(
            !text.contains("Failed to show the cursor"),
            "backend cleanup printed after terminal restoration: {text}"
        );
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
        for sequence in ["\u{1b}[?1000h", "\u{1b}[?1006h"] {
            assert!(text.contains(sequence), "{mode} enabled mouse reporting");
        }
        for sequence in ["\u{1b}[?1006l", "\u{1b}[?1000l"] {
            assert!(text.contains(sequence), "{mode} restored mouse reporting");
        }
        assert!(
            !text.contains("\u{1b}[?1003h"),
            "motion tracking is unnecessary"
        );
        if mode == "panic" {
            assert!(text.contains("previous panic hook ran"));
        }
        let expected = if mode == "normal" {
            vec![
                "push".into(),
                "set Saya | normal 🌸".into(),
                "pop".into(),
                "push".into(),
                "set Saya | next".into(),
                "pop".into(),
            ]
        } else {
            vec!["push".into(), format!("set Saya | {mode}"), "pop".into()]
        };
        assert_eq!(title_operations(&output), expected, "{mode}");
    }
    Ok(())
}

fn title_operations(output: &[u8]) -> Vec<String> {
    let mut operations = Vec::new();
    let mut index = 0;
    while index < output.len() {
        if output[index..].starts_with(b"\x1b[22;2t") {
            operations.push("push".into());
            index += b"\x1b[22;2t".len();
        } else if output[index..].starts_with(b"\x1b[23;2t") {
            operations.push("pop".into());
            index += b"\x1b[23;2t".len();
        } else if output[index..].starts_with(b"\x1b]2;") {
            let start = index + 4;
            let end = output[start..]
                .windows(2)
                .position(|bytes| bytes == b"\x1b\\")
                .unwrap()
                + start;
            operations.push(format!(
                "set {}",
                std::str::from_utf8(&output[start..end]).unwrap()
            ));
            index = end + 2;
        } else {
            index += 1;
        }
    }
    operations
}

#[test]
fn titles_are_protocol_safe_deduplicated_and_utf8_bounded() -> io::Result<()> {
    let _serial = PTY_TESTS.lock().unwrap();
    let output = run_terminal_probe("titles")?;
    let operations = title_operations(&output);
    assert_eq!(operations.len(), 5, "{operations:?}");
    assert_eq!(operations[0], "push");
    assert_eq!(operations[4], "pop");
    assert_eq!(
        operations[1],
        "set Saya | safe]52;c;payload\\ next8;;linkRTLend"
    );
    assert_eq!(operations[2], "set A B");
    let title = operations[3].strip_prefix("set ").unwrap();
    let large = format!("Saya | {}END", "🌸界".repeat(1000));
    assert!(title.len() <= 512);
    assert!(large.starts_with(title));
    assert!(title.len() > 508);
    assert!(!String::from_utf8_lossy(&output).contains("\x1b]52;"));
    assert!(!output.contains(&0x07));
    for operation in operations
        .iter()
        .filter_map(|operation| operation.strip_prefix("set "))
    {
        assert!(!operation.chars().any(|character| character.is_control() || matches!(character, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{206f}')));
    }
    assert!(String::from_utf8_lossy(&output).contains("AFTER_TITLE"));
    Ok(())
}

#[test]
fn slow_render_does_not_consume_terminal_write_budget() -> io::Result<()> {
    let _serial = PTY_TESTS.lock().unwrap();
    let output = run_terminal_probe("render-delay")?;
    assert!(String::from_utf8_lossy(&output).contains("WRITABLE_AFTER_SLOW_RENDER"));
    Ok(())
}

#[test]
fn non_terminal_entry_has_no_terminal_side_effects() -> io::Result<()> {
    let _serial = PTY_TESTS.lock().unwrap();
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

fn fd_flags(fd: i32) -> io::Result<i32> {
    // SAFETY: fcntl reads flags on an existing descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(flags)
    }
}
fn report(path: &Path, value: serde_json::Value) -> io::Result<()> {
    let temporary = path.with_extension("next");
    std::fs::write(
        &temporary,
        serde_json::to_vec(&value).map_err(io::Error::other)?,
    )?;
    std::fs::rename(temporary, path)
}
fn output_fault_child(mode: &str) -> io::Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static HOOK_RAN: AtomicBool = AtomicBool::new(false);
    let path = std::env::var_os("SAYA_TERMINAL_REPORT")
        .ok_or_else(|| io::Error::other("missing probe report"))?;
    let path = Path::new(&path);
    if mode == "closed" {
        // SAFETY: this isolated child handles the peer disappearance explicitly.
        unsafe {
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
        }
    }
    if mode == "slow-panic" {
        std::panic::set_hook(Box::new(|_| {
            HOOK_RAN.store(true, Ordering::Release);
        }));
    }
    let original_flags = fd_flags(libc::STDOUT_FILENO)?;
    let mut terminal = TerminalGuard::enter()?;
    if mode == "drip" {
        report(path, serde_json::json!({"phase":"ready"}))?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !path.with_extension("start").exists() {
            if Instant::now() > deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let text = ("D".repeat(200) + "\n").repeat(160);
        let started = Instant::now();
        let error = terminal
            .draw(|frame| frame.render_widget(ratatui::widgets::Paragraph::new(text), frame.area()))
            .err()
            .ok_or_else(|| io::Error::other("drip output unexpectedly completed"))?;
        let draw_ms = started.elapsed().as_millis();
        let _ = terminal.restore();
        report(
            path,
            serde_json::json!({"phase":"done", "draw_ms":draw_ms,
                "frame_deadline":error.to_string().contains("output write budget"),
                "would_block":error.kind()==io::ErrorKind::WouldBlock,
                "original_flags":original_flags,
                "flags_restored":fd_flags(libc::STDOUT_FILENO)?==original_flags,
                "raw_restored":!crossterm::terminal::is_raw_mode_enabled()?}),
        )?;
        let mut release = String::new();
        std::io::stdin().read_line(&mut release)?;
        return Ok(());
    }
    if mode == "closed" {
        report(path, serde_json::json!({"phase":"ready"}))?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !path.with_extension("release").exists() {
            if Instant::now() > deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let draw_error = terminal
            .draw(|frame| {
                frame.render_widget(
                    ratatui::widgets::Paragraph::new("closed peer"),
                    frame.area(),
                )
            })
            .is_err();
        let started = Instant::now();
        let restore_error = terminal.restore().is_err();
        report(
            path,
            serde_json::json!({"phase":"done", "draw_error":draw_error,
            "restore_error":restore_error, "elapsed_ms":started.elapsed().as_millis(),
            "original_flags":original_flags, "flags_restored":fd_flags(libc::STDOUT_FILENO)?==original_flags,
            "raw_restored":!crossterm::terminal::is_raw_mode_enabled()?}),
        )?;
        // The test harness would write its completion text into the vanished peer.
        std::process::exit(0);
    }
    let started = Instant::now();
    let mut stalled = false;
    for index in 0..2048 {
        let row = if index % 2 == 0 { "A" } else { "B" }.repeat(80) + "\n";
        let text = row.repeat(24);
        match terminal
            .draw(|frame| frame.render_widget(ratatui::widgets::Paragraph::new(text), frame.area()))
        {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                stalled = true;
                break;
            }
            Err(error) => return Err(error),
        }
    }
    assert!(stalled, "probe never filled the terminal output queue");
    let title_started = Instant::now();
    let title_failed = terminal.set_title("Saya | stalled title").is_err();
    let title_ms = title_started.elapsed().as_millis();
    if mode == "slow-panic" {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = terminal.draw(|_| panic!("stalled terminal panic"));
        }));
        assert!(outcome.is_err());
        assert!(HOOK_RAN.load(Ordering::Acquire));
    }
    let restore_started = Instant::now();
    let _ = terminal.restore();
    report(
        path,
        serde_json::json!({"phase":"done", "stalled":stalled,
        "elapsed_ms":started.elapsed().as_millis(), "restore_ms":restore_started.elapsed().as_millis(),
        "title_failed":title_failed,"title_ms":title_ms,
        "original_flags":original_flags, "flags_restored":fd_flags(libc::STDOUT_FILENO)?==original_flags,
        "raw_restored":!crossterm::terminal::is_raw_mode_enabled()?, "hook_ran":HOOK_RAN.load(Ordering::Acquire)}),
    )?;
    let mut release = String::new();
    std::io::stdin().read_line(&mut release)?;
    Ok(())
}
fn wait_for_report(path: &Path, phase: &str, child: &mut Child) -> io::Result<serde_json::Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(bytes) = std::fs::read(path) {
            let value: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            if value["phase"] == phase {
                return Ok(value);
            }
        }
        if let Some(status) = child.try_wait()? {
            return Err(io::Error::other(format!(
                "probe exited before {phase}: {status}"
            )));
        }
        if Instant::now() > deadline {
            child.kill()?;
            child.wait()?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "probe report timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn stalled_terminal_and_panic_restore_raw_and_descriptor_flags_with_bounded_output()
-> io::Result<()> {
    let _serial = PTY_TESTS.lock().unwrap();
    for mode in ["slow", "slow-panic"] {
        let root = tempfile::tempdir()?;
        let path = root.path().join("report.json");
        let PtyProbe {
            mut master,
            slave,
            mut child,
            original,
        } = spawn_probe(mode, Some(&path))?;
        // No read of the master occurs until draw and restoration have completed.
        let report = wait_for_report(&path, "done", &mut child)?;
        assert_eq!(report["stalled"], true);
        assert_eq!(report["flags_restored"], true);
        assert_eq!(report["raw_restored"], true);
        assert_eq!(report["title_failed"], true);
        assert!(report["title_ms"].as_u64().unwrap() < 500);
        assert!(report["elapsed_ms"].as_u64().unwrap() < 3000);
        assert!(report["restore_ms"].as_u64().unwrap() < 500);
        let restored = terminal_attributes(&slave)?;
        assert_eq!(restored.c_iflag, original.c_iflag);
        assert_eq!(restored.c_oflag, original.c_oflag);
        assert_eq!(restored.c_cflag, original.c_cflag);
        assert_eq!(restored.c_lflag, original.c_lflag);
        assert_eq!(restored.c_cc, original.c_cc);
        assert_eq!(
            fd_flags(slave.as_raw_fd())?,
            report["original_flags"].as_i64().unwrap() as i32
        );
        let mut output = Vec::new();
        drain_terminal(&mut master, &mut output)?;
        master.write_all(b"release\n")?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            drain_terminal(&mut master, &mut output)?;
            if let Some(status) = child.try_wait()? {
                let text = String::from_utf8_lossy(&output);
                assert!(status.success(), "{mode}: {}", text);
                assert!(
                    !text.contains("Failed to show the cursor"),
                    "{mode}: {text}"
                );
                break;
            }
            if Instant::now() > deadline {
                child.kill()?;
                child.wait()?;
                return Err(io::ErrorKind::TimedOut.into());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    Ok(())
}
#[test]
fn disconnected_peer_returns_and_restores_original_descriptor_flags() -> io::Result<()> {
    let _serial = PTY_TESTS.lock().unwrap();
    let root = tempfile::tempdir()?;
    let path = root.path().join("report.json");
    let PtyProbe {
        master,
        slave,
        mut child,
        ..
    } = spawn_probe("closed", Some(&path))?;
    wait_for_report(&path, "ready", &mut child)?;
    drop(master);
    std::fs::write(path.with_extension("release"), b"release")?;
    let report = wait_for_report(&path, "done", &mut child)?;
    assert_eq!(report["draw_error"], true);
    assert_eq!(report["flags_restored"], true);
    assert!(report["elapsed_ms"].as_u64().unwrap() < 500);
    assert_eq!(
        fd_flags(slave.as_raw_fd())?,
        report["original_flags"].as_i64().unwrap() as i32
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait()? {
            assert!(status.success());
            break;
        }
        if Instant::now() > deadline {
            child.kill()?;
            child.wait()?;
            return Err(io::ErrorKind::TimedOut.into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

#[test]
fn continuously_slow_output_obeys_one_frame_deadline() -> io::Result<()> {
    let _serial = PTY_TESTS.lock().unwrap();
    let root = tempfile::tempdir()?;
    let path = root.path().join("report.json");
    let PtyProbe {
        mut master,
        slave,
        mut child,
        original,
    } = spawn_probe("drip", Some(&path))?;
    wait_for_report(&path, "ready", &mut child)?;
    let size = libc::winsize {
        ws_col: 200,
        ws_row: 160,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: the live slave descriptor accepts a complete winsize value.
    if unsafe { libc::ioctl(slave.as_raw_fd(), libc::TIOCSWINSZ as _, &size) } < 0 {
        return Err(io::Error::last_os_error());
    }
    std::fs::write(path.with_extension("start"), b"start")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut chunk = [0; 256];
    let value = loop {
        // Keep making progress faster than the per-write retry interval while
        // limiting total throughput, exercising the separate frame deadline.
        match master.read(&mut chunk) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
        if let Ok(bytes) = std::fs::read(&path) {
            let value: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            if value["phase"] == "done" {
                break value;
            }
        }
        if child.try_wait()?.is_some() || Instant::now() > deadline {
            let _ = child.kill();
            // SAFETY: discard pending test output so Darwin can finish exit.
            unsafe { libc::tcflush(slave.as_raw_fd(), libc::TCIOFLUSH) };
            let _ = child.wait();
            return Err(io::Error::other("drip probe did not finish one frame"));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(value["frame_deadline"], true, "{value}");
    assert_eq!(value["would_block"], true);
    assert_eq!(value["raw_restored"], true);
    assert_eq!(value["flags_restored"], true);
    assert!(value["draw_ms"].as_u64().unwrap() < 600, "{value}");
    let restored = terminal_attributes(&slave)?;
    assert_eq!(restored.c_iflag, original.c_iflag);
    assert_eq!(restored.c_oflag, original.c_oflag);
    assert_eq!(restored.c_cflag, original.c_cflag);
    assert_eq!(restored.c_lflag, original.c_lflag);
    assert_eq!(restored.c_cc, original.c_cc);
    let mut output = Vec::new();
    drain_terminal(&mut master, &mut output)?;
    master.write_all(b"release\n")?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        drain_terminal(&mut master, &mut output)?;
        if let Some(status) = child.try_wait()? {
            assert!(status.success(), "{status}");
            break;
        }
        if Instant::now() >= deadline {
            child.kill()?;
            // SAFETY: discard pending test output so Darwin can finish exit.
            unsafe { libc::tcflush(slave.as_raw_fd(), libc::TCIOFLUSH) };
            child.wait()?;
            return Err(io::ErrorKind::TimedOut.into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}
