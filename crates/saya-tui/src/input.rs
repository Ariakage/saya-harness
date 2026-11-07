//! Bounded terminal input isolated from the asynchronous application executor.
use crate::terminal::{descriptor_flags, set_descriptor_flags};
use crossterm::event::{self, Event};
use std::{
    fs::File,
    io,
    os::fd::{AsRawFd, FromRawFd},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle, ThreadId},
    time::Duration,
};
use tokio::sync::mpsc;

const POLL_INTERVAL: Duration = Duration::from_millis(50);
const INPUT_CAPACITY: usize = 64;
static READER: Mutex<Option<Weak<Reader>>> = Mutex::new(None);

/// One terminal reader owned by the frontend's terminal lifetime.
///
/// Create this after entering raw mode. A dedicated thread uses Crossterm's
/// level-triggered terminal polling and sends up to 64 queued events. Input
/// errors are delivered once before the channel closes. Dropping the reader
/// closes the queue and joins the thread after its current poll interval.
/// Terminal restoration also stops this reader before changing termios or FD
/// flags. No other reader may consume Crossterm events while this owner is active.
pub struct TerminalInput {
    receiver: mpsc::Receiver<io::Result<Event>>,
    reader: Arc<Reader>,
    _flags: InputFlags,
}

struct Reader {
    stop: Arc<AtomicBool>,
    worker_id: ThreadId,
    worker: Mutex<Option<JoinHandle<()>>>,
}
impl Reader {
    fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        // A reader panic invokes the same terminal hook. It is already
        // unwinding and must not attempt to join its own thread.
        if self.worker_id == thread::current().id() {
            return;
        }
        let mut worker = match self.worker.lock() {
            Ok(worker) => worker,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(worker) = worker.take() {
            // Keep the lock through join. Concurrent restorers must also wait
            // for the reader to finish before changing terminal FD flags.
            let _ = worker.join();
        }
    }
}

/// Stop input before terminal restoration can turn a partial read blocking.
pub(crate) fn stop_active_reader() {
    let reader = match READER.lock() {
        Ok(slot) => slot.as_ref().and_then(Weak::upgrade),
        Err(poisoned) => poisoned.into_inner().as_ref().and_then(Weak::upgrade),
    };
    if let Some(reader) = reader {
        reader.shutdown();
    }
}

struct InputFlags {
    file: File,
    original: i32,
}
impl InputFlags {
    fn enter() -> io::Result<Self> {
        // SAFETY: duplicate standard input into an owned close-on-exec FD.
        let fd = unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_DUPFD_CLOEXEC, 3) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful fcntl returned a descriptor owned by this file.
        let file = unsafe { File::from_raw_fd(fd) };
        let original = descriptor_flags(fd)?;
        if original & libc::O_NONBLOCK == 0 {
            set_descriptor_flags(fd, original | libc::O_NONBLOCK)?;
        }
        Ok(Self { file, original })
    }
}
impl Drop for InputFlags {
    fn drop(&mut self) {
        if self.original & libc::O_NONBLOCK == 0 {
            let _ = set_descriptor_flags(self.file.as_raw_fd(), self.original);
        }
    }
}

impl TerminalInput {
    /// Start terminal input. Thread creation errors are returned synchronously.
    pub fn new() -> io::Result<Self> {
        if thread::panicking() {
            return Err(io::Error::other(
                "cannot start terminal input during unwinding",
            ));
        }
        let mut slot = READER
            .lock()
            .map_err(|_| io::Error::other("terminal input ownership unavailable"))?;
        if slot.as_ref().and_then(Weak::upgrade).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "terminal input is already active",
            ));
        }
        let flags = InputFlags::enter()?;
        let (sender, receiver) = mpsc::channel(INPUT_CAPACITY);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker = thread::Builder::new()
            .name("saya-terminal-input".into())
            .spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    let mut event = match event::poll(POLL_INTERVAL) {
                        Ok(false) => continue,
                        Ok(true) if stopping.load(Ordering::Acquire) => break,
                        Ok(true) => event::read(),
                        Err(error) => Err(error),
                    };
                    let failed = event.is_err();
                    // The panic hook can stop a full queue without borrowing
                    // its receiver. Backpressure remains on this reader thread.
                    loop {
                        if stopping.load(Ordering::Acquire) {
                            return;
                        }
                        match sender.try_send(event) {
                            Ok(()) => break,
                            Err(mpsc::error::TrySendError::Full(pending)) => {
                                event = pending;
                                thread::sleep(Duration::from_millis(2));
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => return,
                        }
                    }
                    if failed {
                        break;
                    }
                }
            })?;
        let reader = Arc::new(Reader {
            stop,
            worker_id: worker.thread().id(),
            worker: Mutex::new(Some(worker)),
        });
        *slot = Some(Arc::downgrade(&reader));
        Ok(Self {
            receiver,
            reader,
            _flags: flags,
        })
    }

    /// Receive one event or input error. Cancellation leaves queued input intact.
    pub async fn next(&mut self) -> Option<io::Result<Event>> {
        self.receiver.recv().await
    }
}

impl Drop for TerminalInput {
    fn drop(&mut self) {
        self.receiver.close();
        self.reader.shutdown();
        let mut slot = match READER.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        if slot
            .as_ref()
            .is_some_and(|reader| reader.ptr_eq(&Arc::downgrade(&self.reader)))
        {
            *slot = None;
        }
    }
}
