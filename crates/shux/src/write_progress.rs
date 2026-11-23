//! Sending to a client that may have stopped reading: give up only when it
//! takes no bytes for a whole stall window, however long the frame.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use futures::{Sink, SinkExt};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_util::sync::CancellationToken;

/// A stream that counts the bytes its peer has taken.
pub struct Metered<S> {
    inner: S,
    taken: Arc<AtomicU64>,
}

impl<S> Metered<S> {
    pub fn new(inner: S) -> (Self, Arc<AtomicU64>) {
        let taken = Arc::new(AtomicU64::new(0));
        (
            Self {
                inner,
                taken: taken.clone(),
            },
            taken,
        )
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Metered<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Metered<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let polled = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = &polled {
            self.taken.fetch_add(*n as u64, Ordering::Relaxed);
        }
        polled
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Sent {
    Done,
    Closed,
    Stalled,
    Cancelled,
}

/// Send `item`, giving up once a full `stall` passes with no byte taken.
pub async fn send_while_progressing<K, T>(
    sink: &mut K,
    item: T,
    taken: &AtomicU64,
    stall: Duration,
    cancel: &CancellationToken,
) -> Sent
where
    K: Sink<T> + Unpin,
{
    let send = sink.send(item);
    tokio::pin!(send);
    let mut seen = taken.load(Ordering::Relaxed);
    let mut check = tokio::time::interval_at(tokio::time::Instant::now() + stall, stall);
    // A late tick must not be followed by an immediate second one: that would
    // judge a reader on a sliver of time.
    check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Sent::Cancelled,
            sent = &mut send => return if sent.is_ok() { Sent::Done } else { Sent::Closed },
            _ = check.tick() => {
                let now = taken.load(Ordering::Relaxed);
                if now == seen {
                    return Sent::Stalled;
                }
                seen = now;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use tokio::io::AsyncReadExt;
    use tokio_util::codec::{FramedWrite, LengthDelimitedCodec};

    const STALL: Duration = Duration::from_secs(10);

    /// A 64 KiB frame through a 1 KiB pipe; the peer reads 1 KiB a second,
    /// so the frame takes ~64 s to deliver.
    fn slow_link() -> (
        FramedWrite<Metered<tokio::io::DuplexStream>, LengthDelimitedCodec>,
        Arc<AtomicU64>,
        tokio::io::DuplexStream,
    ) {
        let (ours, theirs) = tokio::io::duplex(1024);
        let (metered, taken) = Metered::new(ours);
        (
            FramedWrite::new(metered, LengthDelimitedCodec::new()),
            taken,
            theirs,
        )
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_reader_gets_the_whole_frame() {
        let (mut sink, taken, mut peer) = slow_link();
        let reader = tokio::spawn(async move {
            let mut got = 0usize;
            let mut buf = [0u8; 1024];
            // The link's pace, on the paused clock: 1 KiB a second.
            let mut pace = tokio::time::interval(Duration::from_secs(1));
            loop {
                pace.tick().await;
                match peer.read(&mut buf).await {
                    Ok(0) | Err(_) => return got,
                    Ok(n) => got += n,
                }
            }
        });
        let frame = Bytes::from(vec![b'x'; 64 * 1024]);
        let sent =
            send_while_progressing(&mut sink, frame, &taken, STALL, &CancellationToken::new())
                .await;
        assert_eq!(
            sent,
            Sent::Done,
            "a reader that keeps taking bytes was dropped"
        );
        drop(sink);
        assert_eq!(reader.await.expect("reader"), 64 * 1024 + 4);
    }

    #[tokio::test(start_paused = true)]
    async fn a_reader_that_stops_is_dropped() {
        let (mut sink, taken, _peer) = slow_link();
        let frame = Bytes::from(vec![b'x'; 64 * 1024]);
        let sent =
            send_while_progressing(&mut sink, frame, &taken, STALL, &CancellationToken::new())
                .await;
        assert_eq!(sent, Sent::Stalled);
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_ends_a_stuck_send() {
        let (mut sink, taken, _peer) = slow_link();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let frame = Bytes::from(vec![b'x'; 64 * 1024]);
        let sent = send_while_progressing(&mut sink, frame, &taken, STALL, &cancel).await;
        assert_eq!(sent, Sent::Cancelled);
    }
}
