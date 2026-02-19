//! Client-side daemon auto-start and connection logic.
//!
//! The CLI probes the UDS. On `ConnectionRefused` or `NotFound`, it spawns
//! the daemon via re-exec, then retries with exponential backoff (PRD 4.5).

use std::io;
use std::path::Path;
use std::time::Duration;

use tokio::net::UnixStream;
use tracing::{debug, info};

use crate::daemon::{self, DaemonError};

/// Maximum number of retries when waiting for the daemon to start.
const MAX_CONNECT_RETRIES: u32 = 10;

/// Initial backoff delay between connection attempts.
const INITIAL_BACKOFF: Duration = Duration::from_millis(50);

/// Maximum backoff delay.
const MAX_BACKOFF: Duration = Duration::from_millis(2000);

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("daemon error: {0}")]
    Daemon(#[from] DaemonError),
    #[error("failed to connect to daemon after {0} retries")]
    ConnectionFailed(u32),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Probe the daemon socket. Returns a connected `UnixStream` if successful.
async fn probe_socket(socket_path: &Path) -> Result<UnixStream, io::Error> {
    UnixStream::connect(socket_path).await
}

/// Ensure the daemon is running and return a connected socket.
///
/// 1. Try to connect to the UDS
/// 2. If ConnectionRefused or NotFound, fork the daemon
/// 3. Retry with exponential backoff until connected or max retries
pub async fn ensure_daemon_running() -> Result<UnixStream, ClientError> {
    let sock_path = daemon::socket_path()?;

    // First attempt — maybe daemon is already running
    match probe_socket(&sock_path).await {
        Ok(stream) => {
            debug!("Connected to existing daemon");
            return Ok(stream);
        }
        Err(e) if is_connection_refused_or_not_found(&e) => {
            info!("Daemon not running, starting...");
        }
        Err(e) => return Err(ClientError::Io(e)),
    }

    // Spawn the daemon process
    start_daemon_process()?;

    // Retry with exponential backoff
    let mut backoff = INITIAL_BACKOFF;
    for attempt in 1..=MAX_CONNECT_RETRIES {
        tokio::time::sleep(backoff).await;

        match probe_socket(&sock_path).await {
            Ok(stream) => {
                info!(attempt, "Connected to daemon");
                return Ok(stream);
            }
            Err(e) if is_connection_refused_or_not_found(&e) => {
                debug!(
                    attempt,
                    backoff_ms = backoff.as_millis(),
                    "Daemon not ready yet, retrying..."
                );
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
            Err(e) => return Err(ClientError::Io(e)),
        }
    }

    Err(ClientError::ConnectionFailed(MAX_CONNECT_RETRIES))
}

/// Start the daemon process by re-executing the current binary with
/// an internal `__daemon` subcommand.
///
/// We use re-exec rather than in-process fork+daemonize because the client
/// may already have a tokio runtime running. The `__daemon` subcommand
/// calls `daemonize()` before creating any runtime.
fn start_daemon_process() -> Result<(), ClientError> {
    let exe = std::env::current_exe().map_err(ClientError::Io)?;

    let mut cmd = std::process::Command::new(exe);
    cmd.arg("__daemon");

    // Detach: don't inherit stdin/stdout/stderr
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    cmd.spawn().map_err(ClientError::Io)?;

    Ok(())
}

fn is_connection_refused_or_not_found(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
    )
}
