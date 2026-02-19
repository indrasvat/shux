# 001 — Daemon Skeleton and Process Lifecycle

**Status:** Done
**Depends On:** 000
**Parallelizable With:** 002, 005, 006

---

## Problem

shux is a client/server architecture where the daemon process manages all state, PTYs, and plugins. The daemon must start transparently on first CLI invocation, run as a proper Unix daemon (detached from the controlling terminal), handle signals for graceful shutdown and config reload, and auto-exit when no sessions or daemon leases remain. Getting daemonization wrong leads to zombie processes, orphaned PTYs, or undefined behavior from forking inside a multi-threaded tokio runtime. This task establishes the foundational daemon lifecycle that every subsequent subsystem depends on.

## PRD Reference

- **4.1** Single binary, client/server — daemon auto-starts on first use, auto-exits when last session destroyed
- **4.5** Daemon lifecycle — fork-before-tokio, double-fork with setsid, auto-start/auto-exit, daemon leases, signal handling, CancellationToken
- **4.3** Architectural invariants — single source of truth (daemon owns all state)

---

## Files to Create

- `crates/shux-core/src/daemon.rs` — Core daemon logic: DaemonState, auto-exit timer, lease tracking, CancellationToken tree
- `crates/shux/src/daemon.rs` — Binary-side daemon code: daemonize (double-fork), PID file, socket path, signal handlers, tokio runtime bootstrap
- `crates/shux/src/client.rs` — Client-side probe-and-start logic: UDS probe, daemon spawn, retry with backoff

## Files to Modify

- `crates/shux-core/src/lib.rs` — Add `pub mod daemon;`
- `crates/shux-core/Cargo.toml` — Add dependencies: `tokio`, `tokio-util`, `tracing`, `thiserror`, `uuid`
- `crates/shux/src/main.rs` — Wire up daemon start/client connect entrypoints
- `crates/shux/Cargo.toml` — Add dependencies: `nix`, `tokio`, `tokio-util`, `tracing`

---

## Execution Steps

### Step 1: Define core daemon types in `crates/shux-core/src/daemon.rs`

This module defines the runtime state and lifecycle primitives that the daemon manages. It does NOT handle the Unix daemonization (fork/setsid) — that belongs in the binary crate.

```rust
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, Notify};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// How long to wait after the last session is destroyed before auto-exit.
const AUTO_EXIT_GRACE_PERIOD: Duration = Duration::from_secs(5);

/// Unique identifier for a daemon lease held by a long-lived plugin.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct LeaseId(pub String);

/// Commands sent to the daemon state owner task.
#[derive(Debug)]
pub enum DaemonCommand {
    /// A session was created (increment active session count).
    SessionCreated,
    /// A session was destroyed (decrement active session count).
    SessionDestroyed,
    /// A plugin acquires a daemon lease (prevents auto-exit).
    AcquireLease(LeaseId),
    /// A plugin releases a daemon lease.
    ReleaseLease(LeaseId),
    /// Request graceful shutdown.
    Shutdown,
    /// Trigger config reload.
    ReloadConfig,
}

/// Result of checking whether the daemon should auto-exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitDecision {
    /// Keep running — sessions or leases still active.
    KeepRunning,
    /// Start the grace timer — no sessions, no leases, but give time for reconnection.
    StartGraceTimer,
    /// Shut down now — grace timer expired with no new sessions or leases.
    Exit,
}

/// The daemon's runtime state, managed by a single owner task.
///
/// All mutations arrive via `DaemonCommand` over an mpsc channel.
/// This enforces the single-writer invariant from PRD 4.3 / 4.6.
pub struct DaemonState {
    /// Number of active sessions.
    session_count: u64,
    /// Active daemon leases held by plugins (PRD 4.5).
    leases: HashSet<LeaseId>,
    /// Monotonic version counter for state changes.
    version: AtomicU64,
}

impl DaemonState {
    pub fn new() -> Self {
        Self {
            session_count: 0,
            leases: HashSet::new(),
            version: AtomicU64::new(0),
        }
    }

    pub fn session_count(&self) -> u64 {
        self.session_count
    }

    pub fn lease_count(&self) -> usize {
        self.leases.len()
    }

    pub fn increment_sessions(&mut self) {
        self.session_count += 1;
        self.version.fetch_add(1, Ordering::SeqCst);
    }

    pub fn decrement_sessions(&mut self) {
        self.session_count = self.session_count.saturating_sub(1);
        self.version.fetch_add(1, Ordering::SeqCst);
    }

    pub fn acquire_lease(&mut self, id: LeaseId) {
        self.leases.insert(id);
        self.version.fetch_add(1, Ordering::SeqCst);
    }

    pub fn release_lease(&mut self, id: &LeaseId) {
        self.leases.remove(id);
        self.version.fetch_add(1, Ordering::SeqCst);
    }

    /// Check whether the daemon should consider auto-exit.
    pub fn should_exit(&self) -> ExitDecision {
        if self.session_count == 0 && self.leases.is_empty() {
            ExitDecision::StartGraceTimer
        } else {
            ExitDecision::KeepRunning
        }
    }
}

impl Default for DaemonState {
    fn default() -> Self {
        Self::new()
    }
}
```

Define the daemon run loop that processes commands and manages the auto-exit timer:

```rust
/// The CancellationToken tree for the daemon.
///
/// The root token is cancelled on shutdown. Subsystem tokens are children
/// of the root, so cancelling the root cascades to all subsystems.
///
/// ```text
///   root_token
///   ├── pty_manager_token
///   ├── api_server_token
///   ├── event_bus_token
///   ├── plugin_host_token
///   └── render_token
/// ```
#[derive(Clone)]
pub struct ShutdownTokens {
    /// Root cancellation token. Cancel this to shut down everything.
    pub root: CancellationToken,
}

impl ShutdownTokens {
    pub fn new() -> Self {
        Self {
            root: CancellationToken::new(),
        }
    }

    /// Create a child token for a subsystem. Automatically cancelled
    /// when the root token is cancelled.
    pub fn child(&self) -> CancellationToken {
        self.root.child_token()
    }
}

impl Default for ShutdownTokens {
    fn default() -> Self {
        Self::new()
    }
}

/// Run the daemon state owner task.
///
/// This is the single-writer task that owns `DaemonState`. All mutations
/// flow through the `cmd_rx` channel. This task also manages the auto-exit
/// grace timer per PRD 4.5.
pub async fn run_daemon_state_loop(
    mut cmd_rx: mpsc::Receiver<DaemonCommand>,
    tokens: ShutdownTokens,
    config_reload_notify: Arc<Notify>,
) {
    let mut state = DaemonState::new();
    let mut grace_timer: Option<tokio::time::Sleep> = None;

    loop {
        tokio::select! {
            // Handle incoming commands
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(DaemonCommand::SessionCreated) => {
                        state.increment_sessions();
                        // Cancel any pending grace timer
                        grace_timer = None;
                        info!(sessions = state.session_count(), "Session created");
                    }
                    Some(DaemonCommand::SessionDestroyed) => {
                        state.decrement_sessions();
                        info!(sessions = state.session_count(), "Session destroyed");
                        if state.should_exit() == ExitDecision::StartGraceTimer {
                            info!("No sessions or leases remain, starting {AUTO_EXIT_GRACE_PERIOD:?} grace timer");
                            grace_timer = Some(tokio::time::sleep(AUTO_EXIT_GRACE_PERIOD));
                        }
                    }
                    Some(DaemonCommand::AcquireLease(id)) => {
                        info!(lease = %id.0, "Daemon lease acquired");
                        state.acquire_lease(id);
                        grace_timer = None;
                    }
                    Some(DaemonCommand::ReleaseLease(id)) => {
                        info!(lease = %id.0, "Daemon lease released");
                        state.release_lease(&id);
                        if state.should_exit() == ExitDecision::StartGraceTimer {
                            info!("No sessions or leases remain, starting {AUTO_EXIT_GRACE_PERIOD:?} grace timer");
                            grace_timer = Some(tokio::time::sleep(AUTO_EXIT_GRACE_PERIOD));
                        }
                    }
                    Some(DaemonCommand::Shutdown) => {
                        info!("Explicit shutdown requested");
                        tokens.root.cancel();
                        break;
                    }
                    Some(DaemonCommand::ReloadConfig) => {
                        info!("Config reload requested");
                        config_reload_notify.notify_waiters();
                    }
                    None => {
                        // All senders dropped — shut down
                        warn!("All command senders dropped, shutting down");
                        tokens.root.cancel();
                        break;
                    }
                }
            }

            // Grace timer expired — auto-exit
            _ = async {
                if let Some(ref mut timer) = grace_timer {
                    timer.await;
                } else {
                    // No timer active — wait forever (will be pre-empted by cmd branch)
                    std::future::pending::<()>().await;
                }
            } => {
                // Re-check in case a session was created while timer was running
                if state.should_exit() == ExitDecision::StartGraceTimer {
                    info!("Grace timer expired with no sessions or leases — shutting down");
                    tokens.root.cancel();
                    break;
                } else {
                    grace_timer = None;
                }
            }

            // Root token cancelled externally (e.g., by signal handler)
            _ = tokens.root.cancelled() => {
                info!("Root cancellation token triggered — shutting down");
                break;
            }
        }
    }

    info!("Daemon state loop exiting");
}
```

### Step 2: Define runtime directory helpers in `crates/shux/src/daemon.rs`

This module handles the OS-level daemon lifecycle: paths, PID file, daemonization, and signal handling.

```rust
use std::fs;
use std::io;
use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("failed to determine runtime directory")]
    RuntimeDir,
    #[error("failed to create runtime directory: {0}")]
    CreateDir(io::Error),
    #[error("failed to write PID file: {0}")]
    PidFile(io::Error),
    #[error("failed to remove PID file: {0}")]
    RemovePidFile(io::Error),
    #[error("failed to bind socket: {0}")]
    SocketBind(io::Error),
    #[error("fork failed: {0}")]
    Fork(nix::Error),
    #[error("setsid failed: {0}")]
    Setsid(nix::Error),
    #[error("daemon already running (PID {0})")]
    AlreadyRunning(u32),
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
    let dir = tmpdir.join(format!("shux-{}", uid));
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
```

### Step 3: Implement fork-before-tokio daemonization

This is the critical daemonization function. It MUST run before any tokio runtime is created, because `fork()` in a multi-threaded process is undefined behavior (PRD 4.5).

```rust
/// Daemonize the current process using the double-fork pattern.
///
/// CRITICAL: This function MUST be called BEFORE `tokio::runtime::Runtime::new()`
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
    use nix::unistd::{fork, setsid, ForkResult};

    // First fork
    match unsafe { fork() }.map_err(DaemonError::Fork)? {
        ForkResult::Parent { .. } => {
            // Parent: return false — caller should exit cleanly
            return Ok(false);
        }
        ForkResult::Child => {
            // Child continues below
        }
    }

    // Create new session — detach from controlling terminal
    setsid().map_err(DaemonError::Setsid)?;

    // Second fork — prevent acquiring a controlling terminal
    match unsafe { fork() }.map_err(DaemonError::Fork)? {
        ForkResult::Parent { .. } => {
            // Intermediate child exits
            std::process::exit(0);
        }
        ForkResult::Child => {
            // Grandchild: this is the daemon process
        }
    }

    // Redirect stdio to /dev/null
    redirect_stdio_to_devnull();

    // Ensure runtime dir exists and write PID file
    ensure_runtime_dir()?;
    write_pid_file()?;

    Ok(true)
}

/// Redirect stdin, stdout, stderr to /dev/null.
///
/// This prevents the daemon from accidentally writing to a terminal
/// that may have been closed.
fn redirect_stdio_to_devnull() {
    use std::fs::OpenOptions;
    use std::os::unix::io::AsRawFd;

    if let Ok(devnull) = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
    {
        let fd = devnull.as_raw_d();
        // These are best-effort; if they fail, we continue anyway
        let _ = nix::unistd::dup2(fd, 0); // stdin
        let _ = nix::unistd::dup2(fd, 1); // stdout
        let _ = nix::unistd::dup2(fd, 2); // stderr
    }
}
```

### Step 4: Implement signal handling

Register signal handlers that integrate with the CancellationToken tree. This runs inside the tokio runtime (after fork).

```rust
use std::sync::Arc;
use tokio::sync::{mpsc, Notify};

use shux_core::daemon::{DaemonCommand, ShutdownTokens};

/// Spawn a task that listens for Unix signals and dispatches DaemonCommands.
///
/// - SIGTERM / SIGINT → DaemonCommand::Shutdown (graceful via CancellationToken)
/// - SIGHUP → DaemonCommand::ReloadConfig
pub async fn spawn_signal_handler(
    cmd_tx: mpsc::Sender<DaemonCommand>,
    tokens: ShutdownTokens,
) -> Result<(), DaemonError> {
    use tokio::signal::unix::{signal, SignalKind};

    let mut sigterm = signal(SignalKind::terminate())
        .map_err(DaemonError::Signal)?;
    let mut sigint = signal(SignalKind::interrupt())
        .map_err(DaemonError::Signal)?;
    let mut sighup = signal(SignalKind::hangup())
        .map_err(DaemonError::Signal)?;

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
```

### Step 5: Implement client-side daemon auto-start in `crates/shux/src/client.rs`

The CLI probes the UDS. On `ConnectionRefused` or `NotFound`, it spawns the daemon via `fork()`, then retries with exponential backoff (PRD 4.5).

```rust
use std::io;
use std::path::Path;
use std::time::Duration;

use tokio::net::UnixStream;
use tracing::{debug, info, warn};

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

    // Fork the daemon (must happen before tokio in the forked process,
    // but we are in the parent's tokio — the forked child will create
    // its own runtime after fork)
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
                debug!(attempt, backoff_ms = backoff.as_millis(), "Daemon not ready yet, retrying...");
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

    // Spawn detached process
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
```

### Step 6: Wire up `main.rs` with daemon entrypoint

The `main.rs` must route between client mode (normal CLI) and daemon mode (internal `__daemon` subcommand). The `__daemon` path calls `daemonize()` BEFORE creating the tokio runtime.

```rust
use std::sync::Arc;
use tokio::sync::{mpsc, Notify};

mod client;
mod daemon;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    // Internal daemon subcommand — called by auto-start
    if args.get(1).map(|s| s.as_str()) == Some("__daemon") {
        return run_daemon();
    }

    // Normal CLI client mode
    run_client()
}

/// Daemon entry point.
///
/// 1. Daemonize (double-fork) — BEFORE tokio runtime
/// 2. Create tokio runtime
/// 3. Set up CancellationToken tree
/// 4. Start signal handlers
/// 5. Bind UDS
/// 6. Run daemon state loop
fn run_daemon() -> anyhow::Result<()> {
    // Step 1: Daemonize BEFORE tokio
    if !daemon::daemonize()? {
        // We are the parent — exit cleanly
        return Ok(());
    }

    // Step 2: Now we are the daemon process — create tokio runtime
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        // Initialize tracing (to file, since stdio is /dev/null)
        // TODO: Set up file-based tracing subscriber

        let tokens = shux_core::daemon::ShutdownTokens::new();
        let config_reload_notify = Arc::new(Notify::new());
        let (cmd_tx, cmd_rx) = mpsc::channel(64);

        // Start signal handler
        daemon::spawn_signal_handler(cmd_tx.clone(), tokens.clone()).await?;

        // Ensure runtime dir and clean up stale socket
        daemon::ensure_runtime_dir()?;
        daemon::remove_socket_file()?;

        // TODO (task 008): Bind UDS and start JSON-RPC server
        // For now, bind UDS so client probe succeeds
        let sock_path = daemon::socket_path()?;
        let _listener = tokio::net::UnixListener::bind(&sock_path)?;
        tracing::info!(path = %sock_path.display(), "Daemon listening");

        // Run the daemon state loop (blocks until shutdown)
        shux_core::daemon::run_daemon_state_loop(
            cmd_rx,
            tokens.clone(),
            config_reload_notify,
        ).await;

        // Cleanup
        daemon::remove_pid_file()?;
        daemon::remove_socket_file()?;
        tracing::info!("Daemon shut down cleanly");

        Ok::<(), anyhow::Error>(())
    })?;

    Ok(())
}

/// Client entry point — ensure daemon is running, then execute CLI command.
fn run_client() -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let _stream = client::ensure_daemon_running().await?;

        // TODO (task 011): Parse CLI args with clap, dispatch JSON-RPC calls
        println!("shux v{}", env!("CARGO_PKG_VERSION"));

        Ok::<(), anyhow::Error>(())
    })?;

    Ok(())
}
```

### Step 7: Add `mod daemon;` and `mod client;` to the binary crate

Ensure `crates/shux/src/main.rs` has the module declarations and `crates/shux-core/src/lib.rs` exports the daemon module.

### Step 8: Update Cargo.toml dependencies

**`crates/shux-core/Cargo.toml`** — add:
```toml
[dependencies]
tokio = { workspace = true }
tokio-util = { workspace = true }
tracing = { workspace = true }
thiserror = { workspace = true }
uuid = { workspace = true }
```

**`crates/shux/Cargo.toml`** — add:
```toml
[dependencies]
nix = { workspace = true }
tokio = { workspace = true }
tokio-util = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
anyhow = { workspace = true }
shux-core = { path = "../shux-core" }
```

### Step 9: Write unit tests for DaemonState

In `crates/shux-core/src/daemon.rs`, add a `#[cfg(test)]` module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_state_has_zero_sessions() {
        let state = DaemonState::new();
        assert_eq!(state.session_count(), 0);
        assert_eq!(state.lease_count(), 0);
    }

    #[test]
    fn test_session_increment_decrement() {
        let mut state = DaemonState::new();
        state.increment_sessions();
        state.increment_sessions();
        assert_eq!(state.session_count(), 2);

        state.decrement_sessions();
        assert_eq!(state.session_count(), 1);
    }

    #[test]
    fn test_decrement_saturates_at_zero() {
        let mut state = DaemonState::new();
        state.decrement_sessions();
        assert_eq!(state.session_count(), 0);
    }

    #[test]
    fn test_should_exit_with_sessions() {
        let mut state = DaemonState::new();
        state.increment_sessions();
        assert_eq!(state.should_exit(), ExitDecision::KeepRunning);
    }

    #[test]
    fn test_should_exit_with_leases() {
        let mut state = DaemonState::new();
        state.acquire_lease(LeaseId("mcp-bridge".into()));
        assert_eq!(state.should_exit(), ExitDecision::KeepRunning);
    }

    #[test]
    fn test_should_exit_with_no_sessions_or_leases() {
        let state = DaemonState::new();
        assert_eq!(state.should_exit(), ExitDecision::StartGraceTimer);
    }

    #[test]
    fn test_should_exit_after_all_sessions_destroyed() {
        let mut state = DaemonState::new();
        state.increment_sessions();
        state.decrement_sessions();
        assert_eq!(state.should_exit(), ExitDecision::StartGraceTimer);
    }

    #[test]
    fn test_lease_prevents_exit() {
        let mut state = DaemonState::new();
        state.acquire_lease(LeaseId("mcp".into()));
        assert_eq!(state.should_exit(), ExitDecision::KeepRunning);

        state.release_lease(&LeaseId("mcp".into()));
        assert_eq!(state.should_exit(), ExitDecision::StartGraceTimer);
    }

    #[test]
    fn test_duplicate_lease_is_idempotent() {
        let mut state = DaemonState::new();
        state.acquire_lease(LeaseId("x".into()));
        state.acquire_lease(LeaseId("x".into()));
        assert_eq!(state.lease_count(), 1);
    }

    #[tokio::test]
    async fn test_shutdown_command_cancels_root_token() {
        let tokens = ShutdownTokens::new();
        let notify = Arc::new(Notify::new());
        let (tx, rx) = mpsc::channel(8);

        let tokens_clone = tokens.clone();
        let handle = tokio::spawn(async move {
            run_daemon_state_loop(rx, tokens_clone, notify).await;
        });

        tx.send(DaemonCommand::Shutdown).await.unwrap();
        handle.await.unwrap();

        assert!(tokens.root.is_cancelled());
    }

    #[tokio::test]
    async fn test_grace_timer_triggers_exit() {
        // Use a shorter grace period for testing by directly testing
        // the exit decision logic (the actual timer uses the constant)
        let tokens = ShutdownTokens::new();
        let notify = Arc::new(Notify::new());
        let (tx, rx) = mpsc::channel(8);

        let tokens_clone = tokens.clone();
        let handle = tokio::spawn(async move {
            run_daemon_state_loop(rx, tokens_clone, notify).await;
        });

        // Create and immediately destroy a session
        tx.send(DaemonCommand::SessionCreated).await.unwrap();
        tx.send(DaemonCommand::SessionDestroyed).await.unwrap();

        // Wait for grace timer (5 seconds + buffer)
        tokio::time::sleep(Duration::from_secs(6)).await;

        // The loop should have exited
        assert!(tokens.root.is_cancelled());
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_config_reload_notifies() {
        let tokens = ShutdownTokens::new();
        let notify = Arc::new(Notify::new());
        let (tx, rx) = mpsc::channel(8);

        let notify_clone = notify.clone();
        let tokens_clone = tokens.clone();
        let handle = tokio::spawn(async move {
            run_daemon_state_loop(rx, tokens_clone, notify_clone).await;
        });

        // Send reload and then shutdown
        tx.send(DaemonCommand::ReloadConfig).await.unwrap();
        tx.send(DaemonCommand::Shutdown).await.unwrap();

        handle.await.unwrap();
        assert!(tokens.root.is_cancelled());
    }

    #[test]
    fn test_shutdown_tokens_child_cancelled_with_root() {
        let tokens = ShutdownTokens::new();
        let child = tokens.child();

        assert!(!child.is_cancelled());
        tokens.root.cancel();
        assert!(child.is_cancelled());
    }
}
```

### Step 10: Write integration tests for runtime directory helpers

In `crates/shux/tests/daemon_integration.rs`:

```rust
use std::fs;
use tempfile::TempDir;

/// Test that runtime_dir respects XDG_RUNTIME_DIR
#[test]
fn test_runtime_dir_uses_xdg() {
    let tmpdir = TempDir::new().unwrap();
    std::env::set_var("XDG_RUNTIME_DIR", tmpdir.path());

    // Note: This test modifies process-global env var.
    // In production, use a test harness that isolates env.
    let dir = shux::daemon::runtime_dir().unwrap();
    assert_eq!(dir, tmpdir.path().join("shux"));
}

/// Test PID file round-trip
#[test]
fn test_pid_file_write_read() {
    let tmpdir = TempDir::new().unwrap();
    std::env::set_var("XDG_RUNTIME_DIR", tmpdir.path());

    shux::daemon::ensure_runtime_dir().unwrap();
    shux::daemon::write_pid_file().unwrap();

    let pid = shux::daemon::read_pid_file().unwrap();
    assert_eq!(pid, Some(std::process::id()));

    shux::daemon::remove_pid_file().unwrap();
    let pid = shux::daemon::read_pid_file().unwrap();
    assert!(pid.is_none());
}
```

---

## Verification

### Functional

```bash
# Build the workspace
cargo build --workspace 2>&1 | tail -1
# Expected: "Finished ..."

# Run the daemon in foreground mode (for testing; add --foreground flag later)
# For now, verify the binary compiles and runs
cargo run -p shux
# Expected: "shux v0.1.0"

# Verify runtime directory creation
cargo run -p shux -- __daemon &
DAEMON_PID=$!
sleep 1

# Check PID file exists
cat "${XDG_RUNTIME_DIR:-/tmp/shux-$(id -u)}/shux/shux.pid"

# Check socket exists
ls -la "${XDG_RUNTIME_DIR:-/tmp/shux-$(id -u)}/shux/shux.sock"

# Send SIGTERM for graceful shutdown
kill $DAEMON_PID
sleep 1

# Verify cleanup
test ! -f "${XDG_RUNTIME_DIR:-/tmp/shux-$(id -u)}/shux/shux.pid" && echo "PID file cleaned up"
test ! -S "${XDG_RUNTIME_DIR:-/tmp/shux-$(id -u)}/shux/shux.sock" && echo "Socket cleaned up"
```

### Tests

```bash
# Unit tests for DaemonState
cargo nextest run -p shux-core --lib daemon

# Stale runtime recovery (stale PID/sock should be cleaned and replaced)
cargo nextest run -p shux -- tests::daemon::ensure_running_with_stale_pid

# All workspace tests
cargo nextest run --workspace

# Clippy
cargo clippy --workspace --all-targets -- -D warnings
```

---

## Completion Criteria

- [ ] `crates/shux-core/src/daemon.rs` exists with `DaemonState`, `DaemonCommand`, `ShutdownTokens`, `run_daemon_state_loop`
- [ ] `crates/shux/src/daemon.rs` exists with `runtime_dir()`, `socket_path()`, `pid_file_path()`, `ensure_runtime_dir()`, `write_pid_file()`, `read_pid_file()`, `remove_pid_file()`, `daemonize()`, `spawn_signal_handler()`
- [ ] `crates/shux/src/client.rs` exists with `ensure_daemon_running()` using UDS probe and exponential backoff
- [ ] Double-fork daemonization works (process detaches, PID file written, socket bound)
- [ ] `daemonize()` is called BEFORE tokio runtime creation (fork-before-tokio invariant)
- [ ] SIGTERM triggers graceful shutdown via CancellationToken
- [ ] SIGHUP triggers config reload notification
- [ ] Auto-exit fires after 5-second grace timer when no sessions and no leases
- [ ] Daemon lease acquisition prevents auto-exit
- [ ] PID file and socket file cleaned up on shutdown
- [ ] Runtime directory is `$XDG_RUNTIME_DIR/shux/` with fallback to `$TMPDIR/shux-$UID/`
- [ ] CLI auto-start: `shux` command spawns daemon if not running, retries with backoff
- [ ] `ensure_daemon_running()` handles stale PID/sock files and successfully starts a fresh daemon
- [ ] All unit tests pass (`cargo nextest run -p shux-core`)
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes

---

## Commit Message

```
feat: add daemon skeleton with fork-before-tokio lifecycle

- Double-fork daemonization with setsid (nix crate)
- DaemonState with session counting, lease tracking, auto-exit timer
- CancellationToken tree (tokio-util) for graceful shutdown propagation
- Signal handling: SIGTERM → shutdown, SIGHUP → config reload
- PID file at $XDG_RUNTIME_DIR/shux/shux.pid
- UDS at $XDG_RUNTIME_DIR/shux/shux.sock
- Client auto-start: probe socket, fork daemon, retry with backoff
- Unit tests for DaemonState lifecycle and ShutdownTokens
```

---

## Session Protocol

1. **Before starting:** Read `CLAUDE.md`, `docs/PRD.md` sections 4.1, 4.3, 4.5. Read task 000 to understand the workspace layout. Verify task 000 is complete (workspace builds, all crates exist).
2. **During:** Implement in step order (1-10). After each step, run `cargo check --workspace` to catch compilation errors early. Pay special attention to the fork-before-tokio invariant -- `daemonize()` must be called in `main()` before any `Runtime::new()`.
3. **Testing:** Run `cargo nextest run --workspace` after each major step. The grace timer test (Step 9) takes ~6 seconds -- this is expected.
4. **After:** Run full verification suite. Update `docs/PROGRESS.md` (mark 001 as done, add session log entry). Update `CLAUDE.md` Learnings with any discoveries about nix crate API, fork behavior on macOS, or tokio signal handling.
5. **Watch out for:**
   - macOS does not set `XDG_RUNTIME_DIR` by default -- the fallback path must work
   - `nix::unistd::fork()` requires `unsafe` -- document why it's safe in context
   - `AsRawFd` is being replaced by `AsFd`/`BorrowedFd` in newer Rust -- use the appropriate trait for the nix version
   - The `grace_timer` test is timing-sensitive -- if CI is slow, it may need a longer buffer
