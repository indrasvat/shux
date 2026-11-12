//! Drives [`crate::run_stdio_session`] — the reader/writer split real plugins
//! use — over in-memory pipes. libFuzzer and the unit tests below share it.
//!
//! A one-byte stdin pipe parks the writer on the first reply, which is the
//! case a kernel pipe hides when the burst is smaller than the pipe.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader, ReadBuf};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::{DISCONNECT_REASON, PluginLineHandler, REPLY_BACKLOG, run_stdio_session};

struct Echo {
    resp: mpsc::Sender<String>,
}

impl PluginLineHandler for Echo {
    async fn handle_line(&mut self, line: String) {
        // Same as a synchronous plugin method: stage the reply, never wait
        // for the plugin to read it.
        let _ = self.resp.try_send(format!("REPLY {line}\n"));
    }
}

fn assert_reply_prefix(buf: &[u8]) {
    let text = String::from_utf8_lossy(buf);
    let mut rest = text.as_ref();
    let mut n = 1usize;
    loop {
        let line = format!("REPLY Q{n}");
        let with_nl = format!("{line}\n");
        if let Some(tail) = rest.strip_prefix(with_nl.as_str()) {
            rest = tail;
            n += 1;
            continue;
        }
        assert!(
            rest.is_empty() || line.starts_with(rest),
            "reply stream diverged at {n}: {rest:?}"
        );
        return;
    }
}

fn pipes(
    stdin_pipe: usize,
    stdout_pipe: usize,
) -> (
    tokio::io::DuplexStream,
    tokio::io::DuplexStream,
    tokio::io::DuplexStream,
    tokio::io::DuplexStream,
) {
    let (daemon_read, plugin_write) = tokio::io::duplex(stdout_pipe.max(1));
    let (plugin_read, daemon_write) = tokio::io::duplex(stdin_pipe.max(1));
    (daemon_read, plugin_write, plugin_read, daemon_write)
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

/// Bytes already buffered, or zero if the read would wait.
async fn read_available(read: &mut (impl AsyncRead + Unpin), dst: &mut [u8]) -> usize {
    std::future::poll_fn(|cx| {
        let mut buf = ReadBuf::new(dst);
        match Pin::new(&mut *read).poll_read(cx, &mut buf) {
            Poll::Pending => Poll::Ready(0),
            Poll::Ready(Ok(())) => Poll::Ready(buf.filled().len()),
            Poll::Ready(Err(_)) => Poll::Ready(0),
        }
    })
    .await
}

/// Write `n` requests before reading any reply.
async fn flood_then_read(n: usize, stdin_pipe: usize) -> (Vec<u8>, Option<String>) {
    let (daemon_read, mut plugin_write, mut plugin_read, daemon_write) =
        pipes(stdin_pipe, 1024 * 1024);
    let (inbox_tx, inbox_rx) = mpsc::channel(8);
    let (kill_tx, kill_rx) = oneshot::channel();
    let (resp_tx, resp_rx) = mpsc::channel(64);
    let last_error = Arc::new(Mutex::new(None));
    let session = run_stdio_session(
        "fuzz",
        BufReader::new(daemon_read).lines(),
        daemon_write,
        inbox_rx,
        kill_rx,
        None,
        Arc::new(PathBuf::from("/")),
        Echo { resp: resp_tx },
        resp_rx,
        Arc::clone(&last_error),
        Duration::from_secs(2),
    );
    let peer = async move {
        for i in 1..=n {
            if plugin_write
                .write_all(format!("Q{i}\n").as_bytes())
                .await
                .is_err()
            {
                break;
            }
        }
        // Read before closing stdout. EOF stops a writer parked on the pipe,
        // so closing first would discard replies still queued.
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        while buf.iter().filter(|b| **b == b'\n').count() < n {
            match tokio::time::timeout(Duration::from_secs(2), plugin_read.read(&mut tmp)).await {
                Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
                Ok(Ok(k)) => buf.extend_from_slice(&tmp[..k]),
            }
        }
        drop(plugin_write);
        buf
    };
    let ((), buf) = tokio::join!(
        async move {
            let _ = session.await;
            drop(kill_tx);
            drop(inbox_tx);
        },
        peer
    );
    (buf, last_error.lock().await.clone())
}

/// Write more requests than the queue plus the pipe can hold, and never read.
async fn never_read(stdin_pipe: usize) -> Option<String> {
    let n = REPLY_BACKLOG + stdin_pipe + 16;
    let (daemon_read, mut plugin_write, plugin_read, daemon_write) = pipes(stdin_pipe, 1024 * 1024);
    let (inbox_tx, inbox_rx) = mpsc::channel(8);
    let (kill_tx, kill_rx) = oneshot::channel();
    let (resp_tx, resp_rx) = mpsc::channel(64);
    let last_error = Arc::new(Mutex::new(None));
    let stored = Arc::clone(&last_error);
    let session = async move {
        let _ = run_stdio_session(
            "fuzz",
            BufReader::new(daemon_read).lines(),
            daemon_write,
            inbox_rx,
            kill_rx,
            None,
            Arc::new(PathBuf::from("/")),
            Echo { resp: resp_tx },
            resp_rx,
            last_error,
            Duration::from_millis(200),
        )
        .await;
        drop(kill_tx);
        drop(inbox_tx);
    };
    let peer = async move {
        for i in 1..=n {
            if plugin_write
                .write_all(format!("Q{i}\n").as_bytes())
                .await
                .is_err()
            {
                break;
            }
        }
    };
    tokio::join!(session, peer);
    drop(plugin_read);
    stored.lock().await.clone()
}

/// Flood, or a plugin that never reads. Panics when a reply is reordered, a
/// burst within [`REPLY_BACKLOG`] is short, or the session wedges.
pub fn run_backpressure(data: &[u8]) {
    if data.is_empty() {
        return;
    }
    let rt = runtime();
    // Three shapes, all small enough that a 1-byte pipe still finishes inside
    // the iteration budget. The 2000-reply case is the process test.
    match data[0] % 3 {
        0 => {
            let n = 1 + (data.get(1).copied().unwrap_or(0) as usize % 64);
            let pipe = 1 + (data.get(2).copied().unwrap_or(0) as usize % 8);
            rt.block_on(async {
                let (buf, err) =
                    tokio::time::timeout(Duration::from_secs(2), flood_then_read(n, pipe))
                        .await
                        .expect("flood wedged");
                assert_reply_prefix(&buf);
                assert!(err.is_none(), "burst of {n} disconnected: {err:?}");
                assert_eq!(
                    String::from_utf8_lossy(&buf).lines().count(),
                    n,
                    "burst of {n} on a {pipe}-byte pipe"
                );
            });
        }
        1 => {
            // A 1-byte pipe parks the writer on the first reply, so the queue
            // — not the kernel buffer — is what overflows.
            rt.block_on(async {
                let (buf, err) = tokio::time::timeout(
                    Duration::from_secs(2),
                    flood_then_read(REPLY_BACKLOG + 8, 1),
                )
                .await
                .expect("over-bound flood wedged");
                assert_reply_prefix(&buf);
                assert_eq!(err.as_deref(), Some(DISCONNECT_REASON));
            });
        }
        _ => {
            let pipe = 1 + (data.get(1).copied().unwrap_or(0) as usize % 32);
            rt.block_on(async {
                let err = tokio::time::timeout(Duration::from_secs(2), never_read(pipe))
                    .await
                    .expect("never-read wedged");
                assert_eq!(err.as_deref(), Some(DISCONNECT_REASON));
            });
        }
    }
}

/// Mixed bursts, partial reads, close, and kill. Panics on a reorder or a wedge.
pub fn run_interleave(data: &[u8]) {
    let rt = runtime();
    rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), interleave(data))
            .await
            .expect("interleave wedged")
            .expect("interleave");
    });
}

async fn interleave(data: &[u8]) -> Result<(), String> {
    let pipe = 1 + (data.first().copied().unwrap_or(0) as usize % 64);
    let (daemon_read, plugin_write, mut plugin_read, daemon_write) = pipes(pipe, 256 * 1024);
    let (inbox_tx, inbox_rx) = mpsc::channel(8);
    let (kill_tx, kill_rx) = oneshot::channel();
    let (resp_tx, resp_rx) = mpsc::channel(64);
    let last_error = Arc::new(Mutex::new(None));
    let session = tokio::spawn(run_stdio_session(
        "fuzz",
        BufReader::new(daemon_read).lines(),
        daemon_write,
        inbox_rx,
        kill_rx,
        None,
        Arc::new(PathBuf::from("/")),
        Echo { resp: resp_tx },
        resp_rx,
        last_error,
        Duration::from_millis(200),
    ));

    let mut plugin_write = Some(plugin_write);
    let mut kill_tx = Some(kill_tx);
    let mut next = 1usize;
    let mut buf = Vec::new();
    let mut scratch = [0u8; 128];

    for chunk in data.chunks(2) {
        if session.is_finished() {
            break;
        }
        let op = chunk[0];
        let arg = chunk.get(1).copied().unwrap_or(1);
        match op % 5 {
            0 => {
                let count = 1 + (arg as usize % 40);
                if let Some(write) = plugin_write.as_mut() {
                    for _ in 0..count {
                        if write
                            .write_all(format!("Q{next}\n").as_bytes())
                            .await
                            .is_err()
                        {
                            break;
                        }
                        next += 1;
                    }
                }
            }
            1 | 4 => {
                let n =
                    read_available(&mut plugin_read, &mut scratch[..1 + (arg as usize % 64)]).await;
                buf.extend_from_slice(&scratch[..n]);
            }
            2 => {
                plugin_write.take();
            }
            _ => {
                if let Some(tx) = kill_tx.take() {
                    let _ = tx.send(());
                }
            }
        }
    }

    plugin_write.take();
    let rest = async {
        let mut extra = Vec::new();
        let mut tmp = [0u8; 1024];
        loop {
            match plugin_read.read(&mut tmp).await {
                Ok(0) | Err(_) => break,
                Ok(n) => extra.extend_from_slice(&tmp[..n]),
            }
        }
        extra
    };
    let (joined, extra) = tokio::join!(session, rest);
    drop(kill_tx);
    drop(inbox_tx);
    joined.map_err(|e| format!("session task: {e}"))?;
    buf.extend(extra);
    assert_reply_prefix(&buf);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Closing stdin while stdout stays open must finish the session. The
    /// reply write fails, and a quiet plugin never enqueues another frame, so
    /// the reader has to be told the writer is gone.
    #[tokio::test]
    async fn stdin_close_while_stdout_stays_open_finishes_the_session() {
        let (daemon_read, mut plugin_write, mut plugin_read, daemon_write) = pipes(1, 64 * 1024);
        let (inbox_tx, inbox_rx) = mpsc::channel(8);
        let (kill_tx, kill_rx) = oneshot::channel();
        let (resp_tx, resp_rx) = mpsc::channel(64);
        let last_error = Arc::new(Mutex::new(None));
        let session = tokio::spawn(run_stdio_session(
            "fuzz",
            BufReader::new(daemon_read).lines(),
            daemon_write,
            inbox_rx,
            kill_rx,
            None,
            Arc::new(PathBuf::from("/")),
            Echo { resp: resp_tx },
            resp_rx,
            Arc::clone(&last_error),
            Duration::from_secs(2),
        ));
        plugin_write.write_all(b"Q1\n").await.expect("request");
        let mut one = [0u8; 1];
        plugin_read
            .read_exact(&mut one)
            .await
            .expect("writer started");
        drop(plugin_read);
        let finished = tokio::time::timeout(Duration::from_secs(2), session).await;
        drop(plugin_write);
        drop(inbox_tx);
        drop(kill_tx);
        assert!(
            finished.is_ok(),
            "session wedged after stdin closed while stdout stayed open"
        );
        let err = last_error.lock().await.clone();
        assert!(
            err.as_deref().is_some_and(|e| e.contains("stdin write")),
            "write failure was not reported: {err:?}"
        );
    }

    /// Closing stdout while the writer is parked on a full stdin pipe must
    /// finish the session. The reader is the only task that sees the EOF;
    /// the writer has to be stopped too, or shutdown waits forever.
    #[tokio::test]
    async fn stdout_eof_while_the_writer_is_blocked_finishes_the_session() {
        let (daemon_read, mut plugin_write, mut plugin_read, daemon_write) = pipes(1, 64 * 1024);
        let (inbox_tx, inbox_rx) = mpsc::channel(8);
        let (kill_tx, kill_rx) = oneshot::channel();
        let (resp_tx, resp_rx) = mpsc::channel(64);
        let session = tokio::spawn(run_stdio_session(
            "fuzz",
            BufReader::new(daemon_read).lines(),
            daemon_write,
            inbox_rx,
            kill_rx,
            None,
            Arc::new(PathBuf::from("/")),
            Echo { resp: resp_tx },
            resp_rx,
            Arc::new(Mutex::new(None)),
            Duration::from_secs(2),
        ));
        plugin_write.write_all(b"Q1\n").await.expect("request");
        let mut one = [0u8; 1];
        plugin_read
            .read_exact(&mut one)
            .await
            .expect("writer started");
        drop(plugin_write);
        let finished = tokio::time::timeout(Duration::from_secs(2), session).await;
        drop(plugin_read);
        drop(inbox_tx);
        drop(kill_tx);
        assert!(
            finished.is_ok(),
            "session wedged after stdout EOF while a reply write was blocked"
        );
    }

    /// Kill while the reply queue is already full must still deliver shutdown.
    /// It must not sit behind the queued replies, and it must not be dropped
    /// when `try_send` on that queue fails.
    #[tokio::test]
    async fn kill_while_the_reply_queue_is_full_still_delivers_shutdown() {
        let (daemon_read, mut plugin_write, mut plugin_read, daemon_write) = pipes(1, 1024 * 1024);
        let (inbox_tx, inbox_rx) = mpsc::channel(8);
        let (kill_tx, kill_rx) = oneshot::channel();
        let (resp_tx, resp_rx) = mpsc::channel(64);
        let session = tokio::spawn(run_stdio_session(
            "fuzz",
            BufReader::new(daemon_read).lines(),
            daemon_write,
            inbox_rx,
            kill_rx,
            None,
            Arc::new(PathBuf::from("/")),
            Echo { resp: resp_tx },
            resp_rx,
            Arc::new(Mutex::new(None)),
            Duration::from_secs(2),
        ));
        // One frame is stuck in the writer; the channel holds the rest.
        for i in 1..=REPLY_BACKLOG + 1 {
            plugin_write
                .write_all(format!("Q{i}\n").as_bytes())
                .await
                .expect("request");
        }
        let shutdown = "{\"method\":\"plugin.shutdown\"}\n";
        inbox_tx.send(shutdown.to_string()).await.expect("shutdown");
        kill_tx.send(()).expect("kill");
        // Read only a little. Shutdown has to come out before the queued
        // replies; draining the backlog would hide a shutdown stuck at the tail.
        let mut got = String::new();
        let mut tmp = [0u8; 256];
        while got.lines().count() < 8 && !got.lines().any(|l| l == shutdown.trim_end()) {
            match tokio::time::timeout(Duration::from_millis(500), plugin_read.read(&mut tmp)).await
            {
                Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
                Ok(Ok(n)) => got.push_str(&String::from_utf8_lossy(&tmp[..n])),
            }
        }
        drop(plugin_write);
        drop(inbox_tx);
        let _ = session.await;
        assert!(
            got.lines().any(|l| l == shutdown.trim_end()),
            "shutdown not delivered ahead of the reply backlog: {got:?}"
        );
    }

    /// A kill that lands while a frame is parked mid-write still finishes that
    /// frame before the next one, so a line reader sees `plugin.shutdown` alone.
    #[tokio::test]
    async fn kill_during_a_blocked_write_keeps_the_next_frame_on_its_own_line() {
        let (daemon_read, plugin_write, mut plugin_read, daemon_write) = pipes(8, 64);
        let (inbox_tx, inbox_rx) = mpsc::channel(8);
        let (kill_tx, kill_rx) = oneshot::channel();
        let (resp_tx, resp_rx) = mpsc::channel(64);
        let session = tokio::spawn(run_stdio_session(
            "fuzz",
            BufReader::new(daemon_read).lines(),
            daemon_write,
            inbox_rx,
            kill_rx,
            None,
            Arc::new(PathBuf::from("/")),
            Echo { resp: resp_tx },
            resp_rx,
            Arc::new(Mutex::new(None)),
            Duration::from_secs(5),
        ));
        let _stdout = plugin_write;

        let frame = format!("{}\n", "Z".repeat(400));
        let shutdown = "{\"method\":\"plugin.shutdown\"}\n";
        let (got_tx, got_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let reader = tokio::spawn(async move {
            let mut first = [0u8; 1];
            plugin_read
                .read_exact(&mut first)
                .await
                .expect("first byte");
            let _ = got_tx.send(());
            let _ = release_rx.await;
            let mut rest = Vec::new();
            let _ =
                tokio::time::timeout(Duration::from_secs(3), plugin_read.read_to_end(&mut rest))
                    .await;
            (first[0], rest)
        });

        inbox_tx.send(frame.clone()).await.expect("frame");
        got_rx.await.expect("writer never started");
        inbox_tx.send(shutdown.to_string()).await.expect("shutdown");
        kill_tx.send(()).expect("kill");
        release_tx.send(()).expect("release");

        let (first, rest) = reader.await.expect("reader");
        let mut buf = vec![first];
        buf.extend(rest);
        let text = String::from_utf8(buf).expect("utf8");
        assert!(
            text.starts_with(&frame),
            "cut frame not completed: {text:?}"
        );
        assert!(
            text.lines().any(|l| l == shutdown.trim_end()),
            "shutdown not on its own line: {text:?}"
        );
        drop(inbox_tx);
        let _ = tokio::time::timeout(Duration::from_secs(2), session).await;
    }

    #[test]
    fn interleave_seeds_hold() {
        for seed in [
            vec![0u8, 1],
            vec![0, 40, 1, 8, 0, 10, 4, 0],
            vec![3, 1, 0, 5, 2, 0],
            vec![0, 200, 3, 0, 1, 20],
        ] {
            run_interleave(&seed);
        }
    }
}
