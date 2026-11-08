//! Unix termination signals shared by the text CLI entry points.
use tokio::signal::unix::{Signal, SignalKind, signal};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Received {
    Interrupt,
    Terminate,
}

pub struct Signals {
    interrupt: Signal,
    terminate: Signal,
    hangup: Signal,
}
impl Signals {
    pub fn new() -> std::io::Result<Self> {
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
            hangup: signal(SignalKind::hangup())?,
        })
    }
    pub async fn recv(&mut self) -> Option<Received> {
        tokio::select! {
            value = self.interrupt.recv() => value.map(|()| Received::Interrupt),
            value = self.terminate.recv() => value.map(|()| Received::Terminate),
            value = self.hangup.recv() => value.map(|()| Received::Terminate),
        }
    }
}

pub struct CancellationListener(tokio::task::JoinHandle<()>);
impl CancellationListener {
    pub fn start(cancel: std::sync::Arc<super::Cancel>) -> std::io::Result<Self> {
        let mut signals = Signals::new()?;
        Ok(Self(tokio::spawn(async move {
            let mut interrupted = false;
            while let Some(signal) = signals.recv().await {
                if signal == Received::Interrupt && interrupted {
                    std::process::exit(130);
                }
                interrupted |= signal == Received::Interrupt;
                cancel.cancel();
            }
        })))
    }
}
impl Drop for CancellationListener {
    fn drop(&mut self) {
        self.0.abort();
    }
}
