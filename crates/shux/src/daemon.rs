//! OS-level daemon lifecycle: paths, PID file, daemonization, and signal handling.
//!
//! This module handles the Unix-specific aspects of the daemon — double-fork
//! daemonization, runtime directory management, PID files, and signal handlers.

use std::fs;
use std::io;
use std::path::PathBuf;

use thiserror::Error;
use tokio::sync::mpsc;

use shux_core::daemon::{DaemonCommand, ShutdownTokens};

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("failed to create runtime directory: {0}")]
    CreateDir(io::Error),
    #[error("failed to write PID file: {0}")]
    PidFile(io::Error),
    #[error("failed to remove PID file: {0}")]
    RemovePidFile(io::Error),
    #[error("fork failed: {0}")]
    Fork(nix::Error),
    #[error("setsid failed: {0}")]
    Setsid(nix::Error),
    #[error("signal handler registration failed: {0}")]
    Signal(io::Error),
}

/// Resolve the runtime directory for shux.
///
/// Uses `$XDG_RUNTIME_DIR/shux/` if set, otherwise falls back to
/// `$TMPDIR/shux-$UID/` (macOS doesn't set XDG_RUNTIME_DIR by default).
pub fn runtime_dir() -> Result<PathBuf, DaemonError> {
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        let dir = PathBuf::from(xdg).join("shux");
        return Ok(dir);
    }

    // Fallback for macOS and systems without XDG_RUNTIME_DIR
    let uid = nix::unistd::getuid();
    let tmpdir = std::env::temp_dir();
    let dir = tmpdir.join(format!("shux-{uid}"));
    Ok(dir)
}

/// Full path to the PID file: `$RUNTIME_DIR/shux.pid`
pub fn pid_file_path() -> Result<PathBuf, DaemonError> {
    Ok(runtime_dir()?.join("shux.pid"))
}

/// Full path to the Unix domain socket: `$RUNTIME_DIR/shux.sock`
pub fn socket_path() -> Result<PathBuf, DaemonError> {
    Ok(runtime_dir()?.join("shux.sock"))
}

/// Ensure the runtime directory exists with mode 0700.
pub fn ensure_runtime_dir() -> Result<PathBuf, DaemonError> {
    let dir = runtime_dir()?;
    fs::create_dir_all(&dir).map_err(DaemonError::CreateDir)?;

    // Set permissions to 0700 (owner-only) for security
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o700);
        fs::set_permissions(&dir, perms).map_err(DaemonError::CreateDir)?;
    }

    Ok(dir)
}

/// Write the current process PID to the PID file.
pub fn write_pid_file() -> Result<(), DaemonError> {
    let path = pid_file_path()?;
    let pid = std::process::id();
    fs::write(&path, pid.to_string()).map_err(DaemonError::PidFile)?;
    Ok(())
}

/// Read the PID from the PID file, if it exists.
#[allow(dead_code)] // Used in tests; will be used for stale PID detection in future tasks
pub fn read_pid_file() -> Result<Option<u32>, DaemonError> {
    let path = pid_file_path()?;
    match fs::read_to_string(&path) {
        Ok(contents) => {
            let pid = contents.trim().parse::<u32>().ok();
            Ok(pid)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(DaemonError::PidFile(e)),
    }
}

/// Remove the PID file (called on shutdown).
pub fn remove_pid_file() -> Result<(), DaemonError> {
    let path = pid_file_path()?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(DaemonError::RemovePidFile(e)),
    }
}

/// Remove the socket file (called before binding and on shutdown).
pub fn remove_socket_file() -> Result<(), DaemonError> {
    let path = socket_path()?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(DaemonError::RemovePidFile(e)),
    }
}

/// Daemonize the current process using the double-fork pattern.
///
/// **CRITICAL:** This function MUST be called BEFORE `tokio::runtime::Runtime::new()`
/// or `#[tokio::main]`. Forking a multi-threaded process is undefined behavior.
///
/// The double-fork pattern:
/// 1. First fork: parent exits, child continues
/// 2. setsid(): child becomes session leader (detaches from terminal)
/// 3. Second fork: session leader exits, grandchild continues
///    (grandchild cannot accidentally acquire a controlling terminal)
/// 4. Redirect stdin/stdout/stderr to /dev/null
/// 5. Write PID file
///
/// Returns `Ok(true)` in the daemon process, `Ok(false)` in the original parent.
pub fn daemonize() -> Result<bool, DaemonError> {
    use nix::unistd::{ForkResult, fork, setsid};

    // SAFETY: This is called before any tokio runtime is created, so the process
    // is single-threaded. fork() is safe in single-threaded processes.
    match unsafe { fork() }.map_err(DaemonError::Fork)? {
        ForkResult::Parent { .. } => {
            return Ok(false);
        }
        ForkResult::Child => {}
    }

    // Create new session — detach from controlling terminal
    setsid().map_err(DaemonError::Setsid)?;

    // SAFETY: Still single-threaded (no tokio runtime yet). Second fork prevents
    // the daemon from ever acquiring a controlling terminal.
    match unsafe { fork() }.map_err(DaemonError::Fork)? {
        ForkResult::Parent { .. } => {
            // Intermediate child exits
            std::process::exit(0);
        }
        ForkResult::Child => {}
    }

    // Redirect stdio to /dev/null
    redirect_stdio_to_devnull();

    // Ensure runtime dir exists and write PID file
    ensure_runtime_dir()?;
    write_pid_file()?;

    Ok(true)
}

/// Redirect stdin, stdout, stderr to /dev/null.
fn redirect_stdio_to_devnull() {
    use std::os::unix::io::AsRawFd;

    if let Ok(devnull) = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
    {
        let fd = devnull.as_raw_fd();
        // Best-effort: if dup2 fails, continue anyway
        let _ = nix::unistd::dup2(fd, 0); // stdin
        let _ = nix::unistd::dup2(fd, 1); // stdout
        let _ = nix::unistd::dup2(fd, 2); // stderr
    }
}

/// Spawn a task that listens for Unix signals and dispatches DaemonCommands.
///
/// - SIGTERM / SIGINT → `DaemonCommand::Shutdown` (graceful via CancellationToken)
/// - SIGHUP → `DaemonCommand::ReloadConfig`
pub async fn spawn_signal_handler(
    cmd_tx: mpsc::Sender<DaemonCommand>,
    tokens: ShutdownTokens,
) -> Result<(), DaemonError> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigterm = signal(SignalKind::terminate()).map_err(DaemonError::Signal)?;
    let mut sigint = signal(SignalKind::interrupt()).map_err(DaemonError::Signal)?;
    let mut sighup = signal(SignalKind::hangup()).map_err(DaemonError::Signal)?;

    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = sigterm.recv() => {
                    tracing::info!("Received SIGTERM — initiating graceful shutdown");
                    let _ = cmd_tx.send(DaemonCommand::Shutdown).await;
                    break;
                }
                _ = sigint.recv() => {
                    tracing::info!("Received SIGINT — initiating graceful shutdown");
                    let _ = cmd_tx.send(DaemonCommand::Shutdown).await;
                    break;
                }
                _ = sighup.recv() => {
                    tracing::info!("Received SIGHUP — triggering config reload");
                    let _ = cmd_tx.send(DaemonCommand::ReloadConfig).await;
                }
                _ = tokens.root.cancelled() => {
                    tracing::debug!("Signal handler shutting down (root token cancelled)");
                    break;
                }
            }
        }
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to set XDG_RUNTIME_DIR for testing.
    ///
    /// SAFETY: These tests must run with `--test-threads=1` or use unique
    /// temp dirs to avoid races. `set_var`/`remove_var` are unsafe in
    /// edition 2024 because env vars are process-global shared mutable state.
    unsafe fn set_xdg_runtime_dir(path: impl AsRef<std::ffi::OsStr>) {
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", path) };
    }

    unsafe fn restore_xdg_runtime_dir(original: Option<String>) {
        match original {
            Some(val) => unsafe { std::env::set_var("XDG_RUNTIME_DIR", val) },
            None => unsafe { std::env::remove_var("XDG_RUNTIME_DIR") },
        }
    }

    #[test]
    fn test_runtime_dir_respects_xdg() {
        let original = std::env::var("XDG_RUNTIME_DIR").ok();
        // SAFETY: Test-only env mutation; tests using env vars are not parallel-safe
        // but each test saves and restores the original value.
        unsafe { set_xdg_runtime_dir("/tmp/test-shux-xdg") };

        let dir = runtime_dir().unwrap();
        assert_eq!(dir, PathBuf::from("/tmp/test-shux-xdg/shux"));

        unsafe { restore_xdg_runtime_dir(original) };
    }

    #[test]
    fn test_runtime_dir_fallback_without_xdg() {
        let original = std::env::var("XDG_RUNTIME_DIR").ok();
        // SAFETY: See above
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };

        let dir = runtime_dir().unwrap();
        let uid = nix::unistd::getuid();
        let expected = std::env::temp_dir().join(format!("shux-{uid}"));
        assert_eq!(dir, expected);

        unsafe { restore_xdg_runtime_dir(original) };
    }

    #[test]
    fn test_pid_file_round_trip() {
        let tmpdir = tempfile::TempDir::new().unwrap();
        let original = std::env::var("XDG_RUNTIME_DIR").ok();
        // SAFETY: See above
        unsafe { set_xdg_runtime_dir(tmpdir.path()) };

        ensure_runtime_dir().unwrap();
        write_pid_file().unwrap();

        let pid = read_pid_file().unwrap();
        assert_eq!(pid, Some(std::process::id()));

        remove_pid_file().unwrap();
        let pid = read_pid_file().unwrap();
        assert!(pid.is_none());

        unsafe { restore_xdg_runtime_dir(original) };
    }

    #[test]
    fn test_remove_nonexistent_pid_file_is_ok() {
        let tmpdir = tempfile::TempDir::new().unwrap();
        let original = std::env::var("XDG_RUNTIME_DIR").ok();
        // SAFETY: See above
        unsafe { set_xdg_runtime_dir(tmpdir.path()) };

        ensure_runtime_dir().unwrap();
        remove_pid_file().unwrap();

        unsafe { restore_xdg_runtime_dir(original) };
    }

    #[test]
    fn test_remove_nonexistent_socket_file_is_ok() {
        let tmpdir = tempfile::TempDir::new().unwrap();
        let original = std::env::var("XDG_RUNTIME_DIR").ok();
        // SAFETY: See above
        unsafe { set_xdg_runtime_dir(tmpdir.path()) };

        ensure_runtime_dir().unwrap();
        remove_socket_file().unwrap();

        unsafe { restore_xdg_runtime_dir(original) };
    }
}
