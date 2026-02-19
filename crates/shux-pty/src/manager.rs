//! PtyManager: coordinates all PTY handles and lifecycle events.

use std::collections::HashMap;
use std::path::PathBuf;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::handle::{PtyConfig, PtyError, PtyHandle, PtySize};

/// Events emitted by the PTY subsystem to notify other subsystems.
#[derive(Debug, Clone)]
pub enum PtyEvent {
    Output {
        pane_id: PaneId,
        data: Vec<u8>,
    },
    Exited {
        pane_id: PaneId,
        exit_code: Option<i32>,
    },
    Restarted {
        pane_id: PaneId,
    },
}

/// Pane ID type (mirrors shux-core::model::PaneId).
/// Defined locally to keep shux-pty independent of shux-core.
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub struct PaneId(pub Uuid);

impl std::fmt::Display for PaneId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Restart policy (mirrors shux-core::model::RestartPolicy).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartPolicy {
    Never,
    OnFail,
    Always,
}

/// Determine whether a child should be restarted based on its policy and exit code.
pub fn should_restart(policy: RestartPolicy, exit_code: Option<i32>) -> bool {
    match (policy, exit_code) {
        (RestartPolicy::Always, Some(_)) => true,
        (RestartPolicy::OnFail, Some(code)) => code != 0,
        _ => false,
    }
}

/// Default read buffer size: 8 KiB.
const DEFAULT_READ_BUFFER_SIZE: usize = 8192;

/// The PTY manager owns all PtyHandles and coordinates their lifecycle.
pub struct PtyManager {
    handles: HashMap<PaneId, PtyHandle>,
    event_tx: mpsc::Sender<PtyEvent>,
}

impl PtyManager {
    pub fn new(event_tx: mpsc::Sender<PtyEvent>) -> Self {
        Self {
            handles: HashMap::new(),
            event_tx,
        }
    }

    /// Spawn a new PTY for a pane.
    pub fn spawn(&mut self, pane_id: PaneId, config: &PtyConfig) -> Result<(), PtyError> {
        let handle = PtyHandle::spawn(config)?;
        info!(%pane_id, pid = handle.pid(), "PTY spawned for pane");
        self.handles.insert(pane_id, handle);
        Ok(())
    }

    pub fn get_mut(&mut self, pane_id: &PaneId) -> Option<&mut PtyHandle> {
        self.handles.get_mut(pane_id)
    }

    pub fn get(&self, pane_id: &PaneId) -> Option<&PtyHandle> {
        self.handles.get(pane_id)
    }

    /// Resize a pane's PTY.
    pub fn resize(&mut self, pane_id: &PaneId, size: PtySize) -> Result<(), PtyError> {
        let handle = self.handles.get_mut(pane_id).ok_or(PtyError::Closed)?;
        handle.resize(size)
    }

    /// Write input to a pane's PTY.
    pub async fn write(&mut self, pane_id: &PaneId, data: &[u8]) -> Result<(), PtyError> {
        let handle = self.handles.get_mut(pane_id).ok_or(PtyError::Closed)?;
        handle.write(data).await?;
        handle.flush().await?;
        Ok(())
    }

    /// Kill a pane's child process and remove the handle.
    pub fn kill(&mut self, pane_id: &PaneId) -> Result<(), PtyError> {
        if let Some(mut handle) = self.handles.remove(pane_id) {
            handle.kill()?;
            info!(%pane_id, "PTY killed");
        }
        Ok(())
    }

    /// Remove a PTY handle without killing.
    pub fn remove(&mut self, pane_id: &PaneId) -> Option<PtyHandle> {
        self.handles.remove(pane_id)
    }

    /// Get the current CWD of a pane's child process.
    pub fn cwd(&self, pane_id: &PaneId) -> Option<PathBuf> {
        self.handles.get(pane_id).map(|h| h.current_cwd())
    }

    pub fn active_count(&self) -> usize {
        self.handles.len()
    }

    pub fn active_panes(&self) -> Vec<PaneId> {
        self.handles.keys().copied().collect()
    }

    /// Get the event sender for spawning read loops.
    pub fn event_sender(&self) -> mpsc::Sender<PtyEvent> {
        self.event_tx.clone()
    }
}

/// Spawn the async read loop for a pane's PTY.
///
/// Reads from the PTY master fd in a loop, emitting `PtyEvent::Output` events.
/// On child exit or cancellation, emits `PtyEvent::Exited`.
pub async fn run_pty_read_loop(
    pane_id: PaneId,
    mut handle: PtyHandle,
    event_tx: mpsc::Sender<PtyEvent>,
    shutdown: CancellationToken,
) {
    let mut buf = vec![0u8; DEFAULT_READ_BUFFER_SIZE];

    loop {
        tokio::select! {
            result = handle.read(&mut buf) => {
                match result {
                    Ok(0) => {
                        debug!(%pane_id, "PTY read EOF");
                        break;
                    }
                    Ok(n) => {
                        let data = buf[..n].to_vec();
                        if event_tx
                            .send(PtyEvent::Output { pane_id, data })
                            .await
                            .is_err()
                        {
                            warn!(%pane_id, "Event receiver dropped, stopping read loop");
                            break;
                        }
                    }
                    Err(e) => {
                        if is_transient_error(&e) {
                            continue;
                        }
                        error!(%pane_id, error = %e, "PTY read error");
                        break;
                    }
                }
            }
            _ = shutdown.cancelled() => {
                debug!(%pane_id, "Read loop cancelled by shutdown token");
                break;
            }
        }
    }

    let exit_code = match handle.wait().await {
        Ok(status) => status.code(),
        Err(e) => {
            warn!(%pane_id, error = %e, "Failed to wait for child");
            None
        }
    };

    let _ = event_tx.send(PtyEvent::Exited { pane_id, exit_code }).await;

    info!(%pane_id, ?exit_code, "PTY read loop exited");
}

fn is_transient_error(e: &PtyError) -> bool {
    match e {
        PtyError::Read(io_err) => matches!(
            io_err.kind(),
            std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
        ),
        _ => false,
    }
}

/// Spawn a replacement child process for a pane that exited.
pub async fn respawn_pty(
    pane_id: PaneId,
    config: &PtyConfig,
    event_tx: mpsc::Sender<PtyEvent>,
) -> Result<PtyHandle, PtyError> {
    info!(%pane_id, "Respawning PTY child");

    let handle = PtyHandle::spawn(config)?;

    let _ = event_tx.send(PtyEvent::Restarted { pane_id }).await;

    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_restart_policy_never() {
        assert!(!should_restart(RestartPolicy::Never, Some(0)));
        assert!(!should_restart(RestartPolicy::Never, Some(1)));
        assert!(!should_restart(RestartPolicy::Never, None));
    }

    #[test]
    fn test_restart_policy_on_fail() {
        assert!(!should_restart(RestartPolicy::OnFail, Some(0)));
        assert!(should_restart(RestartPolicy::OnFail, Some(1)));
        assert!(!should_restart(RestartPolicy::OnFail, None));
    }

    #[test]
    fn test_restart_policy_always() {
        assert!(should_restart(RestartPolicy::Always, Some(0)));
        assert!(should_restart(RestartPolicy::Always, Some(1)));
        assert!(!should_restart(RestartPolicy::Always, None));
    }
}
