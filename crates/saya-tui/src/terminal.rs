//! Owns terminal presentation state for one frontend invocation.
use crossterm::{
    cursor::{Hide, Show},
    event::{DisableBracketedPaste, EnableBracketedPaste},
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
        is_raw_mode_enabled,
    },
};
use ratatui::{Frame, Terminal, backend::CrosstermBackend, layout::Size};
use std::{
    io::{self, IsTerminal, Stdout},
    sync::{
        Once,
        atomic::{AtomicBool, Ordering},
    },
};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static PRESENTATION_STARTED: AtomicBool = AtomicBool::new(false);
static PANICKED: AtomicBool = AtomicBool::new(false);
static PANIC_HOOK: Once = Once::new();

/// A single owner of raw mode, alternate screen, bracketed paste and the cursor.
///
/// Entry requires terminal stdin and stdout. Dropping the guard restores terminal
/// state after normal return or unwinding. The application handles signals by
/// cancelling its work and returning through the same guard lifetime.
pub struct TerminalGuard {
    terminal: Option<Terminal<CrosstermBackend<Stdout>>>,
    restored: bool,
}

fn restore_system_terminal() -> io::Result<()> {
    // Always attempt raw-mode restoration, including after an output error.
    let output = if PRESENTATION_STARTED.load(Ordering::Acquire) {
        let result = execute!(
            io::stdout(),
            DisableBracketedPaste,
            Show,
            LeaveAlternateScreen
        );
        if result.is_ok() {
            PRESENTATION_STARTED.store(false, Ordering::Release);
        }
        result
    } else {
        Ok(())
    };
    let raw = disable_raw_mode();
    raw.and(output)
}

fn install_panic_hook() {
    PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |information| {
            if ACTIVE.load(Ordering::Acquire) && !PANICKED.swap(true, Ordering::AcqRel) {
                let _ = restore_system_terminal();
            }
            previous(information);
        }));
    });
}

impl TerminalGuard {
    /// Enter the terminal presentation mode. Partial initialization errors roll
    /// back through this guard before the error is returned.
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
        if is_raw_mode_enabled()?
            || ACTIVE
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "terminal presentation is already active",
            ));
        }
        PANICKED.store(false, Ordering::Release);
        PRESENTATION_STARTED.store(false, Ordering::Release);
        install_panic_hook();
        let mut guard = Self {
            terminal: None,
            restored: false,
        };
        enable_raw_mode()?;
        PRESENTATION_STARTED.store(true, Ordering::Release);
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableBracketedPaste,
            Hide
        )?;
        guard.terminal = Some(Terminal::new(CrosstermBackend::new(io::stdout()))?);
        Ok(guard)
    }

    /// Draw a complete frontend frame. After a panic this returns an error so
    /// the event loop can end instead of drawing into the restored terminal.
    pub fn draw(&mut self, render: impl FnOnce(&mut Frame<'_>)) -> io::Result<()> {
        self.check_active()?;
        self.terminal
            .as_mut()
            .ok_or_else(|| io::Error::other("terminal initialization is incomplete"))?
            .draw(render)
            .map(|_| ())
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
        if self.restored || PANICKED.load(Ordering::Acquire) {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "terminal presentation has ended",
            ))
        } else {
            Ok(())
        }
    }

    /// Restore the terminal explicitly. A failed attempt remains eligible for
    /// the guard's final best-effort retry when it is dropped.
    pub fn restore(&mut self) -> io::Result<()> {
        if self.restored {
            return Ok(());
        }
        let result = restore_system_terminal();
        if result.is_ok() {
            self.restored = true;
            self.terminal = None;
            ACTIVE.store(false, Ordering::Release);
        }
        result
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.restore().is_err() {
            ACTIVE.store(false, Ordering::Release);
        }
    }
}
