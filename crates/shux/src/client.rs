//! Client-side daemon auto-start and connection logic.
//!
//! The CLI probes the UDS. On `ConnectionRefused` or `NotFound`, it spawns
//! the daemon via re-exec, then retries with exponential backoff (PRD 4.5).
//!
//! On successful connection, the client performs a version handshake: if the
//! running daemon's version differs from the client binary's version (e.g.,
//! after a rebuild), the stale daemon is killed and a fresh one is spawned.

use std::io;
use std::path::Path;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tracing::{debug, info, warn};

use crate::daemon::{self, DaemonError};

/// The version of this binary, baked in at compile time.
const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

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

/// Check the daemon's version via `system.version` RPC call.
/// Returns the version string, or None if the check fails.
async fn check_daemon_version(stream: &mut UnixStream) -> Option<String> {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "version-check",
        "method": "system.version",
        "params": {}
    });
    let payload = serde_json::to_vec(&request).ok()?;

    // Write length-prefixed frame
    let len = payload.len() as u32;
    stream.write_all(&len.to_be_bytes()).await.ok()?;
    stream.write_all(&payload).await.ok()?;
    stream.flush().await.ok()?;

    // Read response
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.ok()?;
    let resp_len = u32::from_be_bytes(len_buf) as usize;
    if resp_len > 1024 * 1024 {
        return None;
    }
    let mut resp_buf = vec![0u8; resp_len];
    stream.read_exact(&mut resp_buf).await.ok()?;

    let response: serde_json::Value = serde_json::from_slice(&resp_buf).ok()?;
    response
        .get("result")
        .and_then(|r| r.get("version"))
        .and_then(|v| v.as_str())
        .map(String::from)
}

/// Kill the running daemon by sending SIGTERM to the PID from the PID file.
fn kill_stale_daemon() -> bool {
    let pid = match daemon::read_pid_file() {
        Ok(Some(pid)) => pid,
        _ => return false,
    };

    // Safety: sending SIGTERM to a known process
    let result = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid as i32),
        nix::sys::signal::Signal::SIGTERM,
    );

    if result.is_ok() {
        info!(pid, "Sent SIGTERM to stale daemon");
        true
    } else {
        debug!(pid, "Failed to send SIGTERM (process may already be gone)");
        // Clean up stale files even if kill failed
        let _ = daemon::remove_pid_file();
        let _ = daemon::remove_socket_file();
        true
    }
}

/// Wait for the old daemon to exit by polling the socket until it disconnects.
async fn wait_for_daemon_exit(socket_path: &Path) {
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if probe_socket(socket_path).await.is_err() {
            return;
        }
    }
    // Force cleanup if daemon didn't exit
    let _ = daemon::remove_socket_file();
    let _ = daemon::remove_pid_file();
}

/// Ensure the daemon is running at the given socket path and return a connected socket.
///
/// 1. Try to connect to the UDS
/// 2. If connected, verify version matches this binary; if stale, restart
/// 3. If ConnectionRefused or NotFound, fork the daemon
/// 4. Retry with exponential backoff until connected or max retries
pub async fn ensure_daemon_running_at(socket_path: &Path) -> Result<UnixStream, ClientError> {
    // First attempt — maybe daemon is already running
    match probe_socket(socket_path).await {
        Ok(mut stream) => {
            debug!("Connected to existing daemon, checking version...");

            if let Some(daemon_version) = check_daemon_version(&mut stream).await {
                if daemon_version == CLIENT_VERSION {
                    debug!(version = CLIENT_VERSION, "Daemon version matches");
                    // Need a fresh connection since we consumed the first one for version check
                    drop(stream);
                    return probe_socket(socket_path).await.map_err(ClientError::Io);
                }
                warn!(
                    daemon_version,
                    client_version = CLIENT_VERSION,
                    "Daemon version mismatch — restarting daemon"
                );
            } else {
                warn!("Could not check daemon version — restarting daemon");
            }

            // Version mismatch or check failed: kill and restart
            drop(stream);
            kill_stale_daemon();
            wait_for_daemon_exit(socket_path).await;
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

        match probe_socket(socket_path).await {
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

/// Ensure the daemon is running (using default socket path) and return a connected socket.
#[allow(dead_code)] // Available for backward compatibility
pub async fn ensure_daemon_running() -> Result<UnixStream, ClientError> {
    let sock_path = daemon::socket_path()?;
    ensure_daemon_running_at(&sock_path).await
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

/// Try to connect to an existing daemon without auto-starting.
/// Returns Ok(stream) if daemon is already running, Err otherwise.
pub async fn try_connect(socket_path: &Path) -> Result<UnixStream, ClientError> {
    probe_socket(socket_path).await.map_err(ClientError::Io)
}

fn is_connection_refused_or_not_found(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
    )
}
