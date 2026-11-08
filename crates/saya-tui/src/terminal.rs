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
const MOUSE_BYTES: &[u8] = b"\x1b[?1000h\x1b[?1006h";
const RESTORE_BYTES: &[u8] = b"\x1b[?1006l\x1b[?1000l\x1b[?2004l\x1b[?25h\x1b[?1049l";
const PUSH_TITLE: &[u8] = b"\x1b[22;2t";
const POP_TITLE: &[u8] = b"\x1b[23;2t";
const MAX_TITLE_BYTES: usize = 512;

/// A single owner of raw mode, alternate screen, bracketed paste, mouse and cursor.
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
    write_deadline: Option<Instant>,
    title_saved: bool,
    title: Option<String>,
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
fn write_until(fd: i32, mut remaining: &[u8], deadline: Instant) -> io::Result<()> {
    while !remaining.is_empty() {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "terminal control write budget exceeded",
            ));
        }
        match write_descriptor(fd, remaining) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(count) => remaining = &remaining[count..],
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn write_restore(fd: i32, title_saved: bool) -> io::Result<()> {
    let deadline = Instant::now() + RESTORE_WAIT;
    // Finish any partially written OSC before screen and title restoration.
    for bytes in [
        b"\x1b\\".as_slice(),
        RESTORE_BYTES,
        if title_saved { POP_TITLE } else { b"" },
    ] {
        write_until(fd, bytes, deadline)?;
    }
    Ok(())
}

pub(crate) fn safe_title(text: &str) -> String {
    let mut title = String::with_capacity(MAX_TITLE_BYTES);
    // Bound normalization even for a long input consisting entirely of controls.
    for character in text.chars().take(4096) {
        let character = if character.is_whitespace() {
            if title.is_empty() || title.ends_with(' ') {
                continue;
            }
            ' '
        } else if character.is_control()
            || matches!(character, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{206f}')
        {
            continue;
        } else {
            character
        };
        if title.len() + character.len_utf8() > MAX_TITLE_BYTES {
            break;
        }
        title.push(character);
    }
    title.truncate(title.trim_end().len());
    title
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
                write_deadline: None,
                title_saved: false,
                title: None,
            }),
        })
    }
    fn write_control(self: &Arc<Self>, bytes: &[u8]) -> io::Result<()> {
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| io::Error::other("terminal output state unavailable"))?;
            if state.ended {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            state.write_deadline = Some(Instant::now() + WRITE_WAIT);
        }
        let result = TerminalOutput(self.clone()).write_all(bytes);
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("terminal output state unavailable"))?;
        state.write_deadline = None;
        result?;
        if state.ended {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        Ok(())
    }
    fn save_title(&self) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("terminal output state unavailable"))?;
        if state.ended {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        // Keep the successful push and its restoration marker under one lock,
        // including when another thread's panic hook requests restoration.
        write_until(
            self.file.as_raw_fd(),
            PUSH_TITLE,
            Instant::now() + WRITE_WAIT,
        )?;
        state.title_saved = true;
        Ok(())
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
            write_restore(self.file.as_raw_fd(), state.title_saved)
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
            // Saya has already attempted bounded cursor/screen restoration.
            // Ratatui's destructor writes another ShowCursor and otherwise
            // logs failures to stderr, which may now be a full blocking TTY.
            // This private backend becomes a sink only during teardown.
            return Ok(bytes.len());
        }
        let write_deadline = Instant::now() + WRITE_WAIT;
        let deadline = state
            .write_deadline
            .map_or(write_deadline, |frame| frame.min(write_deadline));
        loop {
            if state
                .write_deadline
                .is_some_and(|frame| Instant::now() >= frame)
            {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "terminal output write budget exceeded",
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
        // Normal and SGR tracking cover clicks and wheels without motion noise.
        guard.output.write_control(MOUSE_BYTES)?;
        // XTWINOPS saves only the window title, leaving the icon label alone.
        // https://www.invisible-island.net/xterm/ctlseqs/ctlseqs.html
        guard.output.save_title()?;
        guard.terminal = Some(Terminal::new(CrosstermBackend::new(TerminalOutput(
            guard.output.clone(),
        )))?);
        Ok(guard)
    }

    /// Set the window title using a sanitized single-line OSC 2 payload.
    /// Controls and bidi formatting are removed, whitespace is collapsed, and
    /// at most 512 UTF-8 bytes are retained from the first 4096 input scalars.
    /// Repeated normalized titles perform no I/O. The complete write has a
    /// 100 ms budget. Ended guards return BrokenPipe.
    /// Title restoration requires a terminal supporting xterm's title stack.
    pub fn set_title(&mut self, title: &str) -> io::Result<()> {
        self.check_active()?;
        let title = safe_title(title);
        if self
            .output
            .state
            .lock()
            .map_err(|_| io::Error::other("terminal output state unavailable"))?
            .title
            .as_deref()
            == Some(title.as_str())
        {
            return Ok(());
        }
        self.output
            .write_control(format!("\x1b\\\x1b]2;{title}\x1b\\").as_bytes())?;
        self.output
            .state
            .lock()
            .map_err(|_| io::Error::other("terminal output state unavailable"))?
            .title = Some(title);
        Ok(())
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
            .write_deadline = Some(Instant::now() + FRAME_WRITE_WAIT);
        let result = terminal.draw(render).map(|_| ());
        self.output
            .state
            .lock()
            .map_err(|_| io::Error::other("terminal output state unavailable"))?
            .write_deadline = None;
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
