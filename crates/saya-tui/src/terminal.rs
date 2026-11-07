//! Owns terminal presentation state for one frontend invocation.
use crossterm::{
    cursor::Hide,
    event::EnableBracketedPaste,
    execute,
    terminal::{EnterAlternateScreen, disable_raw_mode, enable_raw_mode, is_raw_mode_enabled},
};
use ratatui::{Frame, Terminal, backend::CrosstermBackend, layout::Size};
use std::{
    fs::File,
    io::{self, IsTerminal, Write},
    os::fd::{AsRawFd, FromRawFd},
    sync::{Arc, Mutex, Once, Weak},
    time::{Duration, Instant},
};

static OUTPUT: Mutex<Option<Weak<Output>>> = Mutex::new(None);
static PANIC_HOOK: Once = Once::new();
const RESTORE_WAIT: Duration = Duration::from_millis(100);
const WRITE_WAIT: Duration = Duration::from_millis(100);
const FRAME_WRITE_WAIT: Duration = Duration::from_millis(250);
const RESTORE_BYTES: &[u8] = b"\x1b[?2004l\x1b[?25h\x1b[?1049l";

/// A single owner of raw mode, alternate screen, bracketed paste and the cursor.
///
/// Entry requires terminal stdin and stdout. The application handles signals by
/// cancelling its work and returning through the same guard lifetime. Output
/// uses a private unbuffered descriptor and retries transient backpressure for
/// at most 100 ms per write within a 250 ms frame budget. Restoration always
/// attempts termios and original FD flags, with at most 100 ms for
/// screen-control bytes to leave the process.
pub struct TerminalGuard {
    terminal: Option<Terminal<CrosstermBackend<TerminalOutput>>>,
    output: Arc<Output>,
    restored: bool,
}

struct Output {
    file: File,
    original_flags: i32,
    state: Mutex<OutputState>,
}
struct OutputState {
    ended: bool,
    presentation_started: bool,
    restoration_error: Option<io::ErrorKind>,
    frame_deadline: Option<Instant>,
}
struct TerminalOutput(Arc<Output>);

pub(crate) fn descriptor_flags(fd: i32) -> io::Result<i32> {
    // SAFETY: fcntl reads flags from the supplied descriptor without pointers.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(flags)
    }
}
pub(crate) fn set_descriptor_flags(fd: i32, flags: i32) -> io::Result<()> {
    // SAFETY: fcntl updates flags on the supplied descriptor without pointers.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
fn write_descriptor(fd: i32, bytes: &[u8]) -> io::Result<usize> {
    // SAFETY: bytes provides a valid readable buffer for this write operation.
    let count = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
    if count < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(count as usize)
    }
}
fn write_restore(fd: i32) -> io::Result<()> {
    let deadline = Instant::now() + RESTORE_WAIT;
    let mut remaining = RESTORE_BYTES;
    while !remaining.is_empty() {
        match write_descriptor(fd, remaining) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(count) => remaining = &remaining[count..],
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                if Instant::now() >= deadline {
                    return Err(error);
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
impl Output {
    fn new() -> io::Result<Self> {
        let original_flags = descriptor_flags(libc::STDOUT_FILENO)?;
        // SAFETY: duplicate standard output into an owned close-on-exec FD.
        let fd = unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_DUPFD_CLOEXEC, 3) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful fcntl returned a new descriptor owned by this file.
        let file = unsafe { File::from_raw_fd(fd) };
        set_descriptor_flags(fd, original_flags | libc::O_NONBLOCK)?;
        Ok(Self {
            file,
            original_flags,
            state: Mutex::new(OutputState {
                ended: false,
                presentation_started: false,
                restoration_error: None,
                frame_deadline: None,
            }),
        })
    }
    fn restore(&self) -> io::Result<()> {
        crate::input::stop_active_reader();
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        if state.ended {
            return match state.restoration_error {
                Some(kind) => Err(io::Error::new(
                    kind,
                    "terminal restoration previously failed",
                )),
                None => Ok(()),
            };
        }
        state.ended = true;
        // Writers hold this same short lock, so restoring blocking flags cannot
        // race a fresh frame write after its active-state check.
        let raw = disable_raw_mode();
        let screen = if state.presentation_started {
            write_restore(self.file.as_raw_fd())
        } else {
            Ok(())
        };
        let flags = set_descriptor_flags(self.file.as_raw_fd(), self.original_flags);
        let result = raw.and(screen).and(flags);
        state.restoration_error = result.as_ref().err().map(io::Error::kind);
        result
    }
}
impl Write for TerminalOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let state = self
            .0
            .state
            .lock()
            .map_err(|_| io::Error::other("terminal output state unavailable"))?;
        if state.ended {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let write_deadline = Instant::now() + WRITE_WAIT;
        let deadline = state
            .frame_deadline
            .map_or(write_deadline, |frame| frame.min(write_deadline));
        loop {
            if state
                .frame_deadline
                .is_some_and(|frame| Instant::now() >= frame)
            {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "terminal frame write budget exceeded",
                ));
            }
            match write_descriptor(self.0.file.as_raw_fd(), bytes) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(2));
                }
                result => return result,
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct PanicStderrFlags(i32);
impl PanicStderrFlags {
    fn enter() -> io::Result<Self> {
        let flags = descriptor_flags(libc::STDERR_FILENO)?;
        set_descriptor_flags(libc::STDERR_FILENO, flags | libc::O_NONBLOCK)?;
        Ok(Self(flags))
    }
}
impl Drop for PanicStderrFlags {
    fn drop(&mut self) {
        let _ = set_descriptor_flags(libc::STDERR_FILENO, self.0);
    }
}
fn install_panic_hook() {
    PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |information| {
            let output = OUTPUT
                .lock()
                .ok()
                .and_then(|slot| slot.as_ref().and_then(Weak::upgrade));
            if let Some(output) = output {
                let _ = output.restore();
            }
            // The previous diagnostic hook must not wait on a full terminal.
            let _stderr_flags = PanicStderrFlags::enter().ok();
            previous(information);
        }));
    });
}

impl TerminalGuard {
    /// Enter terminal presentation mode. Partial initialization errors roll back
    /// raw state and descriptor flags before they are returned.
    pub fn enter() -> io::Result<Self> {
        if std::thread::panicking() {
            return Err(io::Error::other("cannot enter terminal during unwinding"));
        }
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "TUI requires terminal stdin and stdout",
            ));
        }
        if is_raw_mode_enabled()? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "terminal presentation is already active",
            ));
        }
        install_panic_hook();
        let output = {
            let mut slot = OUTPUT
                .lock()
                .map_err(|_| io::Error::other("terminal ownership unavailable"))?;
            if slot.as_ref().and_then(Weak::upgrade).is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "terminal presentation is already active",
                ));
            }
            let output = Arc::new(Output::new()?);
            *slot = Some(Arc::downgrade(&output));
            output
        };
        let mut guard = Self {
            terminal: None,
            output,
            restored: false,
        };
        enable_raw_mode()?;
        guard
            .output
            .state
            .lock()
            .map_err(|_| io::Error::other("terminal output state unavailable"))?
            .presentation_started = true;
        execute!(
            TerminalOutput(guard.output.clone()),
            EnterAlternateScreen,
            EnableBracketedPaste,
            Hide
        )?;
        guard.terminal = Some(Terminal::new(CrosstermBackend::new(TerminalOutput(
            guard.output.clone(),
        )))?);
        Ok(guard)
    }

    /// Draw a frame, tolerating transient backpressure for at most 100 ms per
    /// write within a 250 ms frame budget. The render closure is synchronous.
    /// A stalled or closed terminal returns an I/O error so the
    /// application can cancel its owned run before leaving.
    pub fn draw(&mut self, render: impl FnOnce(&mut Frame<'_>)) -> io::Result<()> {
        self.check_active()?;
        let terminal = self
            .terminal
            .as_mut()
            .ok_or_else(|| io::Error::other("terminal initialization is incomplete"))?;
        self.output
            .state
            .lock()
            .map_err(|_| io::Error::other("terminal output state unavailable"))?
            .frame_deadline = Some(Instant::now() + FRAME_WRITE_WAIT);
        let result = terminal.draw(render).map(|_| ());
        self.output
            .state
            .lock()
            .map_err(|_| io::Error::other("terminal output state unavailable"))?
            .frame_deadline = None;
        result
    }
    /// Read the current terminal size, including after a resize event.
    pub fn size(&self) -> io::Result<Size> {
        self.check_active()?;
        self.terminal
            .as_ref()
            .ok_or_else(|| io::Error::other("terminal initialization is incomplete"))?
            .size()
    }
    fn check_active(&self) -> io::Result<()> {
        let state = self
            .output
            .state
            .lock()
            .map_err(|_| io::Error::other("terminal output state unavailable"))?;
        if self.restored || state.ended {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "terminal presentation has ended",
            ))
        } else {
            Ok(())
        }
    }
    /// End ownership after one bounded restoration attempt. Screen output may
    /// fail while termios and original file flags have still been restored.
    pub fn restore(&mut self) -> io::Result<()> {
        if self.restored {
            return Ok(());
        }
        let result = self.output.restore();
        self.restored = true;
        self.terminal = None;
        if let Ok(mut slot) = OUTPUT.lock()
            && slot
                .as_ref()
                .and_then(Weak::upgrade)
                .is_some_and(|output| Arc::ptr_eq(&output, &self.output))
        {
            *slot = None;
        }
        result
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}
