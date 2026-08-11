//! Pane output recording — the writer task behind `pane.record.start` and the
//! `lens.run` cast recorder.
//!
//! A recorder is a channel plus a task: the PTY read loop tees every chunk it
//! receives into [`PaneRecordChunk`]s stamped at the tap, and the task writes
//! them out as raw bytes or asciinema v2. Stamping at the tap rather than at
//! the writer is what keeps cast timing honest under backpressure.

use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, mpsc};

use crate::pane_io::PaneIoState;

pub(crate) const PANE_RECORD_CHANNEL_CAPACITY: usize = 128;
pub(crate) const PANE_RECORD_COMPLETED_TTL: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub(crate) enum PaneRecordChunk {
    /// Raw PTY output with its ARRIVAL instant (task 083 cast — stamped at the tap, not at writer
    /// recv, so backpressure never skews cast timing).
    Bytes {
        data: Vec<u8>,
        at: Instant,
    },
    /// A pane resize with its arrival instant (task 083 cast — emits an asciinema `"r"` event so
    /// replay geometry stays honest; grok's gap). Ignored by the raw writer.
    Resize {
        cols: u16,
        rows: u16,
        at: Instant,
    },
    Finish {
        status: PaneRecordStatus,
    },
}

/// Recorder output format (task 083). `Raw` is byte-identical to the pre-083 lossless recorder
/// (timestamps + resize events ignored). `Cast` emits asciinema v2 with monotonic relative
/// timestamps and resize events, UTF-8-boundary-safe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaneRecordFormat {
    Raw,
    /// asciinema v2, seeded with the pane's dims at record start (the cast header geometry).
    Cast {
        cols: u16,
        rows: u16,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PaneRecordStatus {
    Recording,
    Complete,
    Error,
    Aborted,
}

impl PaneRecordStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            PaneRecordStatus::Recording => "recording",
            PaneRecordStatus::Complete => "complete",
            PaneRecordStatus::Error => "error",
            PaneRecordStatus::Aborted => "aborted",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PaneRecordResult {
    pub(crate) status: PaneRecordStatus,
    pub(crate) bytes_written: u64,
    pub(crate) error: Option<String>,
}

pub(crate) struct PaneRecorder {
    pub(crate) id: uuid::Uuid,
    pub(crate) path: PathBuf,
    pub(crate) sender: mpsc::Sender<PaneRecordChunk>,
    pub(crate) outcome: Arc<StdMutex<PaneRecordResult>>,
    pub(crate) task: tokio::task::JoinHandle<()>,
}

/// Open a cast recorder (task 083) — the `lens.run` arm-at-spawn path (`spawn_scratch_core`)
/// builds one and hands it to [`spawn_pane_pty_with_recorder`] so recording begins before the
/// child's first byte. `overwrite` is always true here (the gate owns the ephemeral `.shux/out/`
/// path). Returns a ready-to-register [`PaneRecorder`].
pub(crate) async fn open_cast_recorder(
    path: PathBuf,
    cols: u16,
    rows: u16,
) -> Result<PaneRecorder, String> {
    let (sender, outcome, task) =
        spawn_pane_recorder(path.clone(), true, PaneRecordFormat::Cast { cols, rows }).await?;
    Ok(PaneRecorder {
        id: uuid::Uuid::new_v4(),
        path,
        sender,
        outcome,
        task,
    })
}

pub(crate) async fn spawn_pane_recorder(
    path: PathBuf,
    overwrite: bool,
    format: PaneRecordFormat,
) -> Result<
    (
        mpsc::Sender<PaneRecordChunk>,
        Arc<StdMutex<PaneRecordResult>>,
        tokio::task::JoinHandle<()>,
    ),
    String,
> {
    use tokio::fs::OpenOptions;
    use tokio::io::AsyncWriteExt;

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            format!(
                "failed to create parent directory {}: {e}",
                parent.display()
            )
        })?;
    }

    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        options.custom_flags(nix::libc::O_NOFOLLOW);
    }
    if overwrite {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    let file = options
        .open(&path)
        .await
        .map_err(|e| format!("failed to open record file {}: {e}", path.display()))?;

    let (tx, mut rx) = mpsc::channel::<PaneRecordChunk>(PANE_RECORD_CHANNEL_CAPACITY);
    let outcome = Arc::new(StdMutex::new(PaneRecordResult {
        status: PaneRecordStatus::Recording,
        bytes_written: 0,
        error: None,
    }));
    let writer_outcome = outcome.clone();
    // Epoch at ARM time (not writer-task start) so the first output event's relative time honestly
    // reflects the child's startup latency (task 083 cast; council: arm at spawn).
    let epoch = Instant::now();
    let task = tokio::spawn(async move {
        let mut file = file;
        let mut bytes_written = 0u64;
        let mut final_status = PaneRecordStatus::Complete;

        // Fail the recording, recording bytes written so far, and stop the writer.
        macro_rules! fail {
            ($file:expr, $written:expr, $msg:expr) => {{
                let mut outcome = writer_outcome.lock().expect("record outcome poisoned");
                outcome.status = PaneRecordStatus::Error;
                outcome.bytes_written = $written;
                outcome.error = Some($msg);
                return;
            }};
        }

        let mut cast = match format {
            PaneRecordFormat::Cast { cols, rows } => {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let header = serde_json::json!({
                    "version": 2, "width": cols, "height": rows, "timestamp": ts
                });
                let line = format!("{header}\n");
                if let Err(e) = file.write_all(line.as_bytes()).await {
                    fail!(
                        file,
                        bytes_written,
                        format!("failed to write cast header: {e}")
                    );
                }
                bytes_written += line.len() as u64;
                Some(shux_vt::CastWriter::new(epoch))
            }
            PaneRecordFormat::Raw => None,
        };

        while let Some(chunk) = rx.recv().await {
            match chunk {
                PaneRecordChunk::Bytes { data, at } => match cast.as_mut() {
                    // Cast: emit an "o" event for the complete-UTF-8 prefix (carry the rest).
                    Some(c) => {
                        if let Some(line) = c.output_line(&data, at) {
                            let line = format!("{line}\n");
                            if let Err(e) = file.write_all(line.as_bytes()).await {
                                fail!(
                                    file,
                                    bytes_written,
                                    format!("failed to write cast chunk: {e}")
                                );
                            }
                            bytes_written += line.len() as u64;
                        }
                    }
                    // Raw: byte-identical to the pre-083 recorder.
                    None => {
                        if let Err(e) = file.write_all(&data).await {
                            fail!(
                                file,
                                bytes_written,
                                format!("failed to write record chunk: {e}")
                            );
                        }
                        bytes_written += data.len() as u64;
                    }
                },
                PaneRecordChunk::Resize { cols, rows, at } => {
                    if let Some(c) = cast.as_mut() {
                        let line = format!("{}\n", c.resize_line(cols, rows, at));
                        if let Err(e) = file.write_all(line.as_bytes()).await {
                            fail!(
                                file,
                                bytes_written,
                                format!("failed to write cast resize: {e}")
                            );
                        }
                        bytes_written += line.len() as u64;
                    }
                    // Raw ignores resize events.
                }
                PaneRecordChunk::Finish { status } => {
                    final_status = status;
                    break;
                }
            }
        }

        // Cast: flush any genuinely-truncated trailing UTF-8 at EOF.
        if let Some(c) = cast.as_mut()
            && let Some(line) = c.flush_line()
        {
            let line = format!("{line}\n");
            if let Err(e) = file.write_all(line.as_bytes()).await {
                fail!(
                    file,
                    bytes_written,
                    format!("failed to flush cast tail: {e}")
                );
            }
            bytes_written += line.len() as u64;
        }

        let flush_error = file.flush().await.err();
        let mut outcome = writer_outcome.lock().expect("record outcome poisoned");
        outcome.bytes_written = bytes_written;
        if let Some(e) = flush_error {
            outcome.status = PaneRecordStatus::Error;
            outcome.error = Some(format!("failed to flush record file: {e}"));
        } else if outcome.status == PaneRecordStatus::Recording {
            outcome.status = final_status;
        }
    });

    Ok((tx, outcome, task))
}

pub(crate) async fn tee_pane_recorders(
    io_state: &Arc<Mutex<PaneIoState>>,
    pane_id: shux_core::model::PaneId,
    data: &[u8],
    shutdown: &tokio_util::sync::CancellationToken,
) {
    let sinks: Vec<(
        mpsc::Sender<PaneRecordChunk>,
        Arc<StdMutex<PaneRecordResult>>,
    )> = {
        let state = io_state.lock().await;
        state
            .recorders
            .get(&pane_id)
            .map(|recorders| {
                recorders
                    .iter()
                    .filter(|r| {
                        r.outcome.lock().expect("record outcome poisoned").status
                            == PaneRecordStatus::Recording
                    })
                    .map(|r| (r.sender.clone(), r.outcome.clone()))
                    .collect()
            })
            .unwrap_or_default()
    };

    // Stamp arrival ONCE for all sinks so a cast's timing reflects when bytes left the fd, not
    // when a backpressured writer accepted them (task 083).
    let at = Instant::now();
    for (sender, outcome) in sinks {
        tokio::select! {
            result = sender.send(PaneRecordChunk::Bytes { data: data.to_vec(), at }) => {
                if result.is_err() {
                    let mut outcome = outcome.lock().expect("record outcome poisoned");
                    if outcome.status == PaneRecordStatus::Recording {
                        outcome.status = PaneRecordStatus::Error;
                        outcome.error = Some("pane recorder writer closed before accepting bytes".to_string());
                    }
                }
            }
            _ = shutdown.cancelled() => {
                let mut outcome = outcome.lock().expect("record outcome poisoned");
                if outcome.status == PaneRecordStatus::Recording {
                    outcome.status = PaneRecordStatus::Aborted;
                    outcome.error = Some("pane shutdown interrupted recorder backpressure".to_string());
                }
                return;
            }
        }
    }
}

pub(crate) async fn finish_pane_recorders(
    io_state: &Arc<Mutex<PaneIoState>>,
    pane_id: shux_core::model::PaneId,
) {
    let senders: Vec<mpsc::Sender<PaneRecordChunk>> = {
        let state = io_state.lock().await;
        state
            .recorders
            .get(&pane_id)
            .map(|recorders| recorders.iter().map(|r| r.sender.clone()).collect())
            .unwrap_or_default()
    };

    for sender in senders {
        let _ = sender
            .send(PaneRecordChunk::Finish {
                status: PaneRecordStatus::Complete,
            })
            .await;
    }
}

/// Tee a pane resize into every active recorder (task 083 cast — a cast writer emits an `"r"`
/// event so replay geometry stays honest; the raw writer ignores it). Routed through the SAME
/// per-recording mpsc as the output tap and from the SAME PTY task, so it interleaves in
/// timestamp order with output for free. Clone senders under the lock, send OUTSIDE it — never
/// hold the io mutex across an `.await` (the daemon's cardinal deadlock). Best-effort: a full or
/// closed channel drops the resize event (a missing cast marker is not worth stalling the pane).
pub(crate) async fn tee_pane_resize_recorders(
    io_state: &Arc<Mutex<PaneIoState>>,
    pane_id: shux_core::model::PaneId,
    cols: u16,
    rows: u16,
) {
    let senders: Vec<mpsc::Sender<PaneRecordChunk>> = {
        let state = io_state.lock().await;
        state
            .recorders
            .get(&pane_id)
            .map(|recorders| {
                recorders
                    .iter()
                    .filter(|r| {
                        r.outcome.lock().expect("record outcome poisoned").status
                            == PaneRecordStatus::Recording
                    })
                    .map(|r| r.sender.clone())
                    .collect()
            })
            .unwrap_or_default()
    };
    let at = Instant::now();
    for sender in senders {
        let _ = sender
            .try_send(PaneRecordChunk::Resize { cols, rows, at })
            .map_err(|_| tracing::debug!(%pane_id, "cast resize event dropped (recorder busy)"));
    }
}
