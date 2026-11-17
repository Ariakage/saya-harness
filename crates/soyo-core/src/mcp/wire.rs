use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, ready};
use tokio::io::AsyncWrite;

#[derive(Debug)]
pub(super) struct OutgoingLimit;
impl std::fmt::Display for OutgoingLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MCP outgoing request exceeds its byte limit")
    }
}
impl std::error::Error for OutgoingLimit {}

/// Buffer one SDK-framed stdio message until its full encoded size is known.
/// FramedWrite sends one newline-delimited message and flushes it under the SDK writer lock.
pub(super) struct WireWriter<W> {
    inner: W,
    pending: Vec<u8>,
    written: usize,
    limit: usize,
}
impl<W> WireWriter<W> {
    pub(super) fn new(inner: W, limit: usize) -> Self {
        Self {
            inner,
            pending: Vec::new(),
            written: 0,
            limit,
        }
    }
}
impl<W: AsyncWrite + Unpin> AsyncWrite for WireWriter<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        if self.written > 0 || self.pending.last() == Some(&b'\n') {
            ready!(self.as_mut().poll_flush(context))?;
        }
        let this = &mut *self;
        if bytes.len() > this.limit.saturating_sub(this.pending.len()) {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                OutgoingLimit,
            )));
        }
        this.pending.extend_from_slice(bytes);
        Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        let this = &mut *self;
        if !this.pending.is_empty() && this.pending.last() != Some(&b'\n') {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MCP outgoing message is incomplete",
            )));
        }
        while this.written < this.pending.len() {
            let written = ready!(
                Pin::new(&mut this.inner).poll_write(context, &this.pending[this.written..])
            )?;
            if written == 0 {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "MCP stdio pipe stopped accepting bytes",
                )));
            }
            this.written += written;
        }
        this.pending.clear();
        this.written = 0;
        Pin::new(&mut this.inner).poll_flush(context)
    }
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        ready!(self.as_mut().poll_flush(context))?;
        Pin::new(&mut self.inner).poll_shutdown(context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn oversized_complete_message_is_rejected_before_any_prefix_is_sent() {
        let (sender, mut receiver) = tokio::io::duplex(64);
        let mut sender = WireWriter::new(sender, 6);
        assert!(sender.write_all(b"1234567\n").await.is_err());
        assert!(
            tokio::time::timeout(Duration::from_millis(10), receiver.read_u8())
                .await
                .is_err()
        );
        sender.write_all(b"{}\n").await.unwrap();
        sender.flush().await.unwrap();
        let mut bytes = [0; 3];
        receiver.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"{}\n");
    }

    #[tokio::test]
    async fn complete_message_handles_partial_pipe_writes_and_shutdown() {
        let (sender, mut receiver) = tokio::io::duplex(1);
        let mut sender = WireWriter::new(sender, 32);
        sender.write_all(b"{\"ok\":true}\n").await.unwrap();
        let mut bytes = [0; 12];
        let (sent, received) = tokio::join!(sender.flush(), receiver.read_exact(&mut bytes));
        sent.unwrap();
        received.unwrap();
        assert_eq!(&bytes, b"{\"ok\":true}\n");
        sender.shutdown().await.unwrap();
        assert_eq!(receiver.read(&mut bytes).await.unwrap(), 0);
    }
}
