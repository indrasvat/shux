# 004 — PTY Manager

**Status:** Done
**Depends On:** 001
**Parallelizable With:** 002, 003, 005

---

## Problem

Every pane in shux runs a child process (usually a shell) inside a pseudo-terminal (PTY). The PTY manager is responsible for spawning child processes, reading their output asynchronously, writing user input to them, resizing the PTY when pane dimensions change, detecting process exit, and implementing restart policies. Without this subsystem, shux cannot display any terminal content -- it is the bridge between the data model and the actual programs the user is running. The implementation must be fully async (tokio), handle process lifecycle robustly, and track the child's current working directory for auto-title and new pane CWD inheritance.

## PRD Reference

- **4.2** System diagram — PTY Manager (pty-proc, async I/O, lifecycle)
- **5.1** Pane entity — pty: PtyHandle, cwd, command, exit_status, restart policy
- **15.2** Key crate families — pty-process 0.5.3 (AsyncRead/AsyncWrite, tokio integration, resize support)
- **6.2** P1 features — Pane respawn (restart=never|on-fail|always)

---

## Files to Create

- `crates/shux-pty/src/lib.rs` — Public module declarations and re-exports
- `crates/shux-pty/src/manager.rs` — PtyManager: spawns/tracks all PTY processes, coordinates lifecycle events
- `crates/shux-pty/src/handle.rs` — PtyHandle: per-pane PTY wrapper with async read/write/resize, CWD tracking, exit detection

## Files to Modify

- `crates/shux-pty/Cargo.toml` — Add dependencies: `pty-process`, `tokio`, `tokio-util`, `nix`, `tracing`, `thiserror`, `uuid`

---

## Execution Steps

### Step 1: Define the PtyHandle in `crates/shux-pty/src/handle.rs`

The `PtyHandle` wraps a single PTY child process. It provides async reading, writing, resizing, and exit status retrieval.

```rust
use std::path::PathBuf;
use std::process::ExitStatus;

use pty_process::Pty;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tracing::{debug, error, info, warn};

/// Errors from PTY operations.
#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    #[error("failed to open PTY: {0}")]
    Open(std::io::Error),

    #[error("failed to spawn child process: {0}")]
    Spawn(std::io::Error),

    #[error("failed to read from PTY: {0}")]
    Read(std::io::Error),

    #[error("failed to write to PTY: {0}")]
    Write(std::io::Error),

    #[error("failed to resize PTY: {0}")]
    Resize(std::io::Error),

    #[error("child process already exited")]
    AlreadyExited,

    #[error("PTY handle closed")]
    Closed,

    #[error("failed to get CWD: {0}")]
    Cwd(std::io::Error),
}

/// Configuration for spawning a PTY child process.
#[derive(Debug, Clone)]
pub struct PtyConfig {
    /// Command to run. If empty, uses the user's default shell.
    pub command: Vec<String>,
    /// Working directory for the child process.
    pub cwd: PathBuf,
    /// Environment variables to set (in addition to inherited env).
    pub env: Vec<(String, String)>,
    /// Initial PTY size (columns x rows).
    pub size: PtySize,
}

/// PTY dimensions in columns and rows.
#[derive(Debug, Clone, Copy)]
pub struct PtySize {
    pub cols: u16,
    pub rows: u16,
}

impl PtySize {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }
}

impl Default for PtySize {
    fn default() -> Self {
        Self { cols: 80, rows: 24 }
    }
}

impl PtyConfig {
    /// Create a config that spawns the user's default shell.
    pub fn default_shell(cwd: PathBuf) -> Self {
        Self {
            command: Vec::new(),
            cwd,
            env: Vec::new(),
            size: PtySize::default(),
        }
    }

    /// Create a config with a specific command.
    pub fn with_command(command: Vec<String>, cwd: PathBuf) -> Self {
        Self {
            command,
            cwd,
            env: Vec::new(),
            size: PtySize::default(),
        }
    }

    /// Resolve the command to run. If empty, returns the user's login shell.
    fn resolve_command(&self) -> Vec<String> {
        if self.command.is_empty() {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
            vec![shell, "-l".to_string()]
        } else {
            self.command.clone()
        }
    }
}
```

### Step 2: Implement PTY spawning

Use the `pty-process` crate to create a PTY pair and spawn the child process.

```rust
/// A handle to a running PTY child process.
///
/// This wraps the PTY master file descriptor and the child process.
/// It provides async read/write via tokio integration and methods
/// for resize, CWD tracking, and exit detection.
pub struct PtyHandle {
    /// The PTY master (for read/write).
    pty: Pty,
    /// The child process.
    child: pty_process::Child,
    /// The child's PID (for CWD tracking and signal sending).
    pid: u32,
    /// The initial working directory.
    initial_cwd: PathBuf,
    /// Current known size.
    size: PtySize,
}

impl PtyHandle {
    /// Spawn a new PTY child process with the given configuration.
    ///
    /// This creates a PTY pair, sets the initial size, and spawns the
    /// child process. The child inherits the PTY slave as its controlling
    /// terminal.
    pub fn spawn(config: &PtyConfig) -> Result<Self, PtyError> {
        // Open a PTY pair
        let pty = Pty::new().map_err(PtyError::Open)?;

        // Set initial size
        pty.resize(pty_process::Size::new(config.size.rows, config.size.cols))
            .map_err(PtyError::Resize)?;

        // Build the command
        let cmd_parts = config.resolve_command();
        let program = &cmd_parts[0];
        let args = &cmd_parts[1..];

        let mut cmd = pty_process::Command::new(program);
        cmd.args(args);
        cmd.current_dir(&config.cwd);

        // Set additional environment variables
        for (key, value) in &config.env {
            cmd.env(key, value);
        }

        // Ensure TERM is set for proper terminal behavior
        cmd.env("TERM", "xterm-256color");

        // Spawn the child
        let child = cmd.spawn(&pty.pts().map_err(PtyError::Open)?)
            .map_err(PtyError::Spawn)?;

        let pid = child.id().unwrap_or(0);

        info!(pid, cmd = ?cmd_parts, cwd = %config.cwd.display(), "PTY child spawned");

        Ok(Self {
            pty,
            child,
            pid,
            initial_cwd: config.cwd.clone(),
            size: config.size,
        })
    }

    /// Get the child's PID.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Get the initial working directory.
    pub fn initial_cwd(&self) -> &PathBuf {
        &self.initial_cwd
    }

    /// Get the current PTY size.
    pub fn size(&self) -> PtySize {
        self.size
    }
}
```

### Step 3: Implement async read/write

The PTY master fd supports `AsyncRead` and `AsyncWrite` through pty-process's tokio integration.

```rust
impl PtyHandle {
    /// Read bytes from the PTY (child's stdout/stderr).
    ///
    /// Returns the number of bytes read, or 0 on EOF (child exited).
    /// The caller should call this in a loop, feeding the bytes to the
    /// VT parser (task 005).
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize, PtyError> {
        self.pty.read(buf).await.map_err(PtyError::Read)
    }

    /// Write bytes to the PTY (child's stdin).
    ///
    /// Used for forwarding user input to the child process.
    pub async fn write(&mut self, data: &[u8]) -> Result<(), PtyError> {
        self.pty.write_all(data).await.map_err(PtyError::Write)
    }

    /// Write a string to the PTY.
    pub async fn write_str(&mut self, text: &str) -> Result<(), PtyError> {
        self.write(text.as_bytes()).await
    }

    /// Flush the write buffer.
    pub async fn flush(&mut self) -> Result<(), PtyError> {
        self.pty.flush().await.map_err(PtyError::Write)
    }
}
```

### Step 4: Implement resize

When a pane's dimensions change (user resizes, window resizes, layout changes), the PTY must be notified via `TIOCSWINSZ` so the child process can adapt.

```rust
impl PtyHandle {
    /// Resize the PTY to new dimensions.
    ///
    /// This sends `TIOCSWINSZ` to the child process (via pty-process),
    /// which in turn sends `SIGWINCH` to the foreground process group.
    /// The child (e.g., vim, less, bash) redraws for the new size.
    pub fn resize(&mut self, new_size: PtySize) -> Result<(), PtyError> {
        self.pty
            .resize(pty_process::Size::new(new_size.rows, new_size.cols))
            .map_err(PtyError::Resize)?;
        self.size = new_size;
        debug!(
            pid = self.pid,
            cols = new_size.cols,
            rows = new_size.rows,
            "PTY resized"
        );
        Ok(())
    }
}
```

### Step 5: Implement process lifecycle and exit detection

```rust
impl PtyHandle {
    /// Wait for the child process to exit.
    ///
    /// Returns the exit status. This should be called in a tokio::select!
    /// alongside the read loop so the caller knows when the child exits.
    pub async fn wait(&mut self) -> Result<ExitStatus, PtyError> {
        let status = self.child.wait().await.map_err(PtyError::Spawn)?;
        info!(pid = self.pid, ?status, "PTY child exited");
        Ok(status)
    }

    /// Try to get the exit status without blocking.
    ///
    /// Returns `Some(status)` if the child has exited, `None` if still running.
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>, PtyError> {
        self.child.try_wait().map_err(PtyError::Spawn)
    }

    /// Send a signal to the child process.
    ///
    /// Commonly used to send SIGTERM for graceful shutdown or SIGHUP
    /// for terminal hangup.
    pub fn kill(&mut self) -> Result<(), PtyError> {
        self.child.kill().map_err(PtyError::Spawn)
    }
}
```

### Step 6: Implement CWD tracking

Track the child process's current working directory by reading from the OS. This is platform-specific.

```rust
impl PtyHandle {
    /// Get the current working directory of the child process.
    ///
    /// This reads from the OS:
    /// - Linux: `/proc/<pid>/cwd` (symlink)
    /// - macOS: `proc_pidinfo` (libproc)
    ///
    /// Returns the initial CWD as fallback if the OS query fails.
    pub fn current_cwd(&self) -> PathBuf {
        self.try_current_cwd().unwrap_or_else(|| self.initial_cwd.clone())
    }

    fn try_current_cwd(&self) -> Option<PathBuf> {
        #[cfg(target_os = "linux")]
        {
            self.cwd_linux()
        }

        #[cfg(target_os = "macos")]
        {
            self.cwd_macos()
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            None
        }
    }

    #[cfg(target_os = "linux")]
    fn cwd_linux(&self) -> Option<PathBuf> {
        let path = format!("/proc/{}/cwd", self.pid);
        std::fs::read_link(&path).ok()
    }

    #[cfg(target_os = "macos")]
    fn cwd_macos(&self) -> Option<PathBuf> {
        use std::ffi::CStr;
        use std::mem;
        use std::os::raw::c_int;

        // Use proc_pidinfo with PROC_PIDVNODEPATHINFO to get the CWD
        // This is equivalent to: lsof -p $PID | grep cwd
        extern "C" {
            fn proc_pidinfo(
                pid: c_int,
                flavor: c_int,
                arg: u64,
                buffer: *mut libc::c_void,
                buffersize: c_int,
            ) -> c_int;
        }

        // PROC_PIDVNODEPATHINFO = 9
        const PROC_PIDVNODEPATHINFO: c_int = 9;

        #[repr(C)]
        struct VnodePathInfo {
            _vip_vi: [u8; 152],     // struct vnode_info_path (we skip most fields)
            vip_path: [u8; 1024],   // MAXPATHLEN
        }

        // Alternative approach: use the `sysinfo` or `procfs` crate
        // For now, use the simple /dev/fd approach via lsof
        // A more robust implementation would use proc_pidinfo directly.
        // As a simpler fallback, try reading /dev/fd/cwd for the process.

        // Simplified approach: use `pwdx` equivalent via procinfo
        // For the initial implementation, return None and fall back
        // to the initial CWD. A future optimization can add proper
        // proc_pidinfo support.
        //
        // TODO: Implement proper proc_pidinfo-based CWD tracking for macOS.
        // For now, use libproc crate if available, or fall back.
        None
    }
}
```

### Step 7: Implement the PtyManager in `crates/shux-pty/src/manager.rs`

The `PtyManager` coordinates all PTY handles, mapping them by PaneId.

```rust
use std::collections::HashMap;
use std::path::PathBuf;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::handle::{PtyConfig, PtyError, PtyHandle, PtySize};

/// Events emitted by the PtyManager to notify other subsystems.
#[derive(Debug, Clone)]
pub enum PtyEvent {
    /// Data was read from a pane's PTY output.
    Output {
        pane_id: PaneId,
        data: Vec<u8>,
    },
    /// A pane's child process exited.
    Exited {
        pane_id: PaneId,
        exit_code: Option<i32>,
    },
    /// A pane's child process was restarted.
    Restarted {
        pane_id: PaneId,
    },
}

/// Pane ID type (mirrors model::PaneId but avoids cross-crate dependency
/// until the crate graph is finalized).
///
/// In practice, this will be re-exported from shux-core::model::PaneId.
/// For now, we define it locally to keep shux-pty independent.
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub struct PaneId(pub Uuid);

impl std::fmt::Display for PaneId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The PTY manager owns all PtyHandles and coordinates their lifecycle.
///
/// It runs a read loop per PTY that feeds output to the VT parser (via
/// events), detects child exit, and handles restart policies.
pub struct PtyManager {
    /// Active PTY handles, indexed by PaneId.
    handles: HashMap<PaneId, PtyHandle>,
    /// Channel for emitting PTY events to other subsystems.
    event_tx: mpsc::Sender<PtyEvent>,
    /// Read buffer size (bytes). Larger = fewer syscalls, more latency.
    read_buffer_size: usize,
}

/// Default read buffer size: 8 KiB.
/// Balances throughput (less syscall overhead) vs latency (faster display of small outputs).
const DEFAULT_READ_BUFFER_SIZE: usize = 8192;

impl PtyManager {
    /// Create a new PtyManager.
    pub fn new(event_tx: mpsc::Sender<PtyEvent>) -> Self {
        Self {
            handles: HashMap::new(),
            event_tx,
            read_buffer_size: DEFAULT_READ_BUFFER_SIZE,
        }
    }

    /// Spawn a new PTY for a pane.
    ///
    /// Creates the PTY, spawns the child process, and starts the
    /// async read loop. Returns the PaneId for tracking.
    pub fn spawn(
        &mut self,
        pane_id: PaneId,
        config: &PtyConfig,
        shutdown: CancellationToken,
    ) -> Result<(), PtyError> {
        let handle = PtyHandle::spawn(config)?;
        info!(%pane_id, pid = handle.pid(), "PTY spawned for pane");
        self.handles.insert(pane_id, handle);
        Ok(())
    }

    /// Get a mutable reference to a PTY handle.
    pub fn get_mut(&mut self, pane_id: &PaneId) -> Option<&mut PtyHandle> {
        self.handles.get_mut(pane_id)
    }

    /// Get an immutable reference to a PTY handle.
    pub fn get(&self, pane_id: &PaneId) -> Option<&PtyHandle> {
        self.handles.get(pane_id)
    }

    /// Resize a pane's PTY.
    pub fn resize(&mut self, pane_id: &PaneId, size: PtySize) -> Result<(), PtyError> {
        let handle = self.handles.get_mut(pane_id).ok_or(PtyError::Closed)?;
        handle.resize(size)
    }

    /// Write input to a pane's PTY.
    pub async fn write(
        &mut self,
        pane_id: &PaneId,
        data: &[u8],
    ) -> Result<(), PtyError> {
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

    /// Remove a PTY handle without killing (for panes whose child already exited).
    pub fn remove(&mut self, pane_id: &PaneId) -> Option<PtyHandle> {
        self.handles.remove(pane_id)
    }

    /// Get the current CWD of a pane's child process.
    pub fn cwd(&self, pane_id: &PaneId) -> Option<PathBuf> {
        self.handles.get(pane_id).map(|h| h.current_cwd())
    }

    /// Number of active PTY handles.
    pub fn active_count(&self) -> usize {
        self.handles.len()
    }

    /// List all active pane IDs.
    pub fn active_panes(&self) -> Vec<PaneId> {
        self.handles.keys().copied().collect()
    }
}
```

### Step 8: Implement the per-pane read loop

Each PTY needs a continuous async read loop that reads output and forwards it to the VT parser. This is spawned as a tokio task per pane.

```rust
/// Spawn the async read loop for a pane's PTY.
///
/// This task reads from the PTY master fd in a loop, emitting PtyEvent::Output
/// events. When the child exits or the cancellation token is triggered, the
/// loop terminates and emits PtyEvent::Exited.
///
/// This function takes ownership of the PtyHandle. The handle is consumed
/// by the task and cannot be used from the PtyManager after this point
/// (except through events).
pub async fn run_pty_read_loop(
    pane_id: PaneId,
    mut handle: PtyHandle,
    event_tx: mpsc::Sender<PtyEvent>,
    shutdown: CancellationToken,
) {
    let mut buf = vec![0u8; DEFAULT_READ_BUFFER_SIZE];

    loop {
        tokio::select! {
            // Read from PTY
            result = handle.read(&mut buf) => {
                match result {
                    Ok(0) => {
                        // EOF — child closed its end
                        debug!(%pane_id, "PTY read EOF");
                        break;
                    }
                    Ok(n) => {
                        let data = buf[..n].to_vec();
                        if event_tx
                            .send(PtyEvent::Output {
                                pane_id,
                                data,
                            })
                            .await
                            .is_err()
                        {
                            warn!(%pane_id, "Event receiver dropped, stopping read loop");
                            break;
                        }
                    }
                    Err(e) => {
                        // EAGAIN/EINTR are transient — retry
                        if is_transient_error(&e) {
                            continue;
                        }
                        error!(%pane_id, error = %e, "PTY read error");
                        break;
                    }
                }
            }

            // Shutdown requested
            _ = shutdown.cancelled() => {
                debug!(%pane_id, "Read loop cancelled by shutdown token");
                break;
            }
        }
    }

    // Wait for the child process to exit (may already be done)
    let exit_code = match handle.wait().await {
        Ok(status) => status.code(),
        Err(e) => {
            warn!(%pane_id, error = %e, "Failed to wait for child");
            None
        }
    };

    let _ = event_tx
        .send(PtyEvent::Exited { pane_id, exit_code })
        .await;

    info!(%pane_id, ?exit_code, "PTY read loop exited");
}

/// Check if an I/O error is transient (should be retried).
fn is_transient_error(e: &PtyError) -> bool {
    match e {
        PtyError::Read(io_err) => matches!(
            io_err.kind(),
            std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
        ),
        _ => false,
    }
}
```

### Step 9: Implement restart policy handler

When a child process exits, the restart policy determines what happens next.

```rust
/// Restart policy for a pane (mirrors model::RestartPolicy).
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

/// Spawn a replacement child process for a pane that exited.
///
/// This creates a new PtyHandle with the same config and starts a new
/// read loop. The caller should update the data model accordingly.
pub async fn respawn_pty(
    pane_id: PaneId,
    config: &PtyConfig,
    event_tx: mpsc::Sender<PtyEvent>,
    shutdown: CancellationToken,
) -> Result<PtyHandle, PtyError> {
    info!(%pane_id, "Respawning PTY child");

    let handle = PtyHandle::spawn(config)?;

    let _ = event_tx
        .send(PtyEvent::Restarted { pane_id })
        .await;

    Ok(handle)
}
```

### Step 10: Set up the crate's public API in `crates/shux-pty/src/lib.rs`

```rust
//! shux-pty — PTY manager for shux
//!
//! This crate manages pseudo-terminal (PTY) child processes for panes.
//! It provides:
//!
//! - `PtyHandle`: per-pane PTY wrapper with async read/write/resize
//! - `PtyManager`: coordinates all PTY handles, lifecycle events
//! - `PtyEvent`: events emitted on output, exit, restart
//! - `PtyConfig`: configuration for spawning child processes
//!
//! The PTY manager integrates with tokio for async I/O and uses the
//! `pty-process` crate for portable PTY operations.

pub mod handle;
pub mod manager;

pub use handle::{PtyConfig, PtyError, PtyHandle, PtySize};
pub use manager::{PaneId, PtyEvent, PtyManager};
```

### Step 11: Update Cargo.toml dependencies

**`crates/shux-pty/Cargo.toml`:**

```toml
[package]
name = "shux-pty"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
pty-process = { workspace = true }
tokio = { workspace = true }
tokio-util = { workspace = true }
nix = { workspace = true }
tracing = { workspace = true }
thiserror = { workspace = true }
uuid = { workspace = true }

# macOS CWD tracking (optional, for proc_pidinfo)
[target.'cfg(target_os = "macos")'.dependencies]
libc = "0.2"

[dev-dependencies]
tokio = { workspace = true, features = ["test-util"] }
tempfile = { workspace = true }
```

### Step 12: Write integration tests

Integration tests spawn real PTY processes and verify output capture, input forwarding, resize, and exit detection.

```rust
// In crates/shux-pty/tests/integration.rs

use std::path::PathBuf;
use std::time::Duration;

use shux_pty::{PtyConfig, PtyHandle, PtySize};
use tokio::io::AsyncReadExt;

fn test_cwd() -> PathBuf {
    std::env::temp_dir()
}

#[tokio::test]
async fn test_spawn_echo() {
    let config = PtyConfig::with_command(
        vec!["echo".into(), "hello shux".into()],
        test_cwd(),
    );

    let mut handle = PtyHandle::spawn(&config).unwrap();
    let mut output = Vec::new();
    let mut buf = [0u8; 1024];

    // Read until EOF or timeout
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match handle.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => output.extend_from_slice(&buf[..n]),
                Err(_) => break,
            }
        }
    })
    .await;

    assert!(result.is_ok(), "Read timed out");

    let output_str = String::from_utf8_lossy(&output);
    assert!(
        output_str.contains("hello shux"),
        "Expected 'hello shux' in output, got: {output_str}"
    );
}

#[tokio::test]
async fn test_spawn_and_exit_status() {
    let config = PtyConfig::with_command(
        vec!["true".into()],
        test_cwd(),
    );

    let mut handle = PtyHandle::spawn(&config).unwrap();
    let status = handle.wait().await.unwrap();
    assert!(status.success(), "Expected exit code 0");
}

#[tokio::test]
async fn test_spawn_failing_command() {
    let config = PtyConfig::with_command(
        vec!["false".into()],
        test_cwd(),
    );

    let mut handle = PtyHandle::spawn(&config).unwrap();
    let status = handle.wait().await.unwrap();
    assert!(!status.success(), "Expected non-zero exit code");
}

#[tokio::test]
async fn test_write_and_read() {
    // Spawn cat, write input, read it back
    let config = PtyConfig::with_command(
        vec!["cat".into()],
        test_cwd(),
    );

    let mut handle = PtyHandle::spawn(&config).unwrap();

    // Write some input
    handle.write(b"hello from test\n").await.unwrap();
    handle.flush().await.unwrap();

    // Read the echoed output (PTY echoes input + cat echoes it again)
    let mut buf = [0u8; 4096];
    let mut output = Vec::new();

    let result = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match handle.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    output.extend_from_slice(&buf[..n]);
                    let s = String::from_utf8_lossy(&output);
                    // cat echoes the input, so we should see "hello from test" in output
                    if s.contains("hello from test") {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    })
    .await;

    assert!(result.is_ok(), "Read timed out");

    let output_str = String::from_utf8_lossy(&output);
    assert!(
        output_str.contains("hello from test"),
        "Expected echoed input, got: {output_str}"
    );

    // Clean up: send EOF and wait
    handle.kill().ok();
}

#[tokio::test]
async fn test_resize() {
    let mut config = PtyConfig::default_shell(test_cwd());
    config.size = PtySize::new(80, 24);

    let mut handle = PtyHandle::spawn(&config).unwrap();
    assert_eq!(handle.size().cols, 80);
    assert_eq!(handle.size().rows, 24);

    // Resize
    handle.resize(PtySize::new(120, 40)).unwrap();
    assert_eq!(handle.size().cols, 120);
    assert_eq!(handle.size().rows, 40);

    // Verify child receives SIGWINCH by checking $COLUMNS in the shell
    // (This is harder to test reliably; the resize syscall success is sufficient)

    handle.kill().ok();
}

#[tokio::test]
async fn test_initial_cwd() {
    let cwd = std::env::temp_dir();
    let config = PtyConfig::default_shell(cwd.clone());

    let handle = PtyHandle::spawn(&config).unwrap();
    assert_eq!(handle.initial_cwd(), &cwd);

    let mut handle = handle;
    handle.kill().ok();
}

#[tokio::test]
async fn test_pty_event_output() {
    use shux_pty::manager::{PaneId, PtyEvent};
    use tokio::sync::mpsc;
    use uuid::Uuid;

    let pane_id = PaneId(Uuid::new_v4());
    let (event_tx, mut event_rx) = mpsc::channel(32);
    let shutdown = tokio_util::sync::CancellationToken::new();

    let config = PtyConfig::with_command(
        vec!["echo".into(), "event test".into()],
        test_cwd(),
    );
    let handle = PtyHandle::spawn(&config).unwrap();

    // Spawn the read loop
    let shutdown_clone = shutdown.clone();
    tokio::spawn(async move {
        shux_pty::manager::run_pty_read_loop(
            pane_id,
            handle,
            event_tx,
            shutdown_clone,
        )
        .await;
    });

    // Collect events
    let mut got_output = false;
    let mut got_exit = false;

    let result = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = event_rx.recv().await {
            match event {
                PtyEvent::Output { pane_id: pid, data } => {
                    assert_eq!(pid, pane_id);
                    let s = String::from_utf8_lossy(&data);
                    if s.contains("event test") {
                        got_output = true;
                    }
                }
                PtyEvent::Exited { pane_id: pid, exit_code } => {
                    assert_eq!(pid, pane_id);
                    assert_eq!(exit_code, Some(0));
                    got_exit = true;
                    break;
                }
                _ => {}
            }
        }
    })
    .await;

    assert!(result.is_ok(), "Event collection timed out");
    assert!(got_output, "Did not receive output event");
    assert!(got_exit, "Did not receive exit event");
}

#[test]
fn test_restart_policy() {
    use shux_pty::manager::{should_restart, RestartPolicy};

    assert!(!should_restart(RestartPolicy::Never, Some(0)));
    assert!(!should_restart(RestartPolicy::Never, Some(1)));
    assert!(!should_restart(RestartPolicy::OnFail, Some(0)));
    assert!(should_restart(RestartPolicy::OnFail, Some(1)));
    assert!(should_restart(RestartPolicy::Always, Some(0)));
    assert!(should_restart(RestartPolicy::Always, Some(1)));
    assert!(!should_restart(RestartPolicy::Always, None)); // still running
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn test_cwd_tracking_linux() {
    use std::time::Duration;

    let config = PtyConfig::default_shell(PathBuf::from("/tmp"));
    let mut handle = PtyHandle::spawn(&config).unwrap();

    // Give the shell time to start
    tokio::time::sleep(Duration::from_millis(500)).await;

    let cwd = handle.current_cwd();
    // The shell should be in /tmp (or the resolved symlink)
    assert!(
        cwd.to_string_lossy().contains("tmp"),
        "Expected CWD to contain 'tmp', got: {}",
        cwd.display()
    );

    handle.kill().ok();
}
```

---

## Verification

### Functional

```bash
# Build
cargo build --workspace

# Verify PTY spawning works
cargo run -p shux-pty --example spawn_echo || echo "No example yet — use tests"

# Quick sanity: spawn a shell and interact (manual test)
# Not automated — just verify the build succeeds
```

### Tests

```bash
# Run unit tests
cargo nextest run -p shux-pty --lib

# Restart policy behavior
cargo nextest run -p shux-pty -- tests::restart_policy::on_fail_vs_always

# Run integration tests (spawns real PTY processes)
cargo nextest run -p shux-pty --test integration

# All tests
cargo nextest run --workspace

# Clippy
cargo clippy --workspace --all-targets -- -D warnings
```

---

## Completion Criteria

- [ ] `crates/shux-pty/src/handle.rs` exists with `PtyHandle`, `PtyConfig`, `PtySize`, `PtyError`
- [ ] `PtyHandle::spawn()` creates a PTY pair and spawns a child process via `pty-process`
- [ ] `PtyHandle::read()` provides async reading from the PTY master
- [ ] `PtyHandle::write()` provides async writing to the PTY master (input forwarding)
- [ ] `PtyHandle::resize()` sends TIOCSWINSZ to the child (via pty-process)
- [ ] `PtyHandle::wait()` returns the child's exit status
- [ ] `PtyHandle::kill()` terminates the child process
- [ ] `PtyHandle::current_cwd()` tracks the child's CWD (Linux: /proc/pid/cwd, macOS: stub with fallback)
- [ ] `crates/shux-pty/src/manager.rs` exists with `PtyManager`, `PtyEvent`, `run_pty_read_loop`
- [ ] `PtyManager` maintains HashMap<PaneId, PtyHandle> for tracking
- [ ] `PtyEvent::Output` emitted when data is read from PTY
- [ ] `PtyEvent::Exited` emitted when child process exits (with exit code)
- [ ] `run_pty_read_loop` reads continuously and respects CancellationToken
- [ ] Restart policy logic: `should_restart()` correctly handles Never/OnFail/Always
- [ ] Integration test verifies `OnFail` restarts only on failure and `Always` restarts on every exit
- [ ] Spawn/open failures return typed errors (`PtyError::Spawn` / `PtyError::Open`) instead of panicking
- [ ] Integration test: spawn `/bin/echo`, verify output contains expected string
- [ ] Integration test: spawn `true` / `false`, verify exit codes
- [ ] Integration test: write to `cat`, verify echoed output
- [ ] Integration test: resize PTY, verify size update
- [ ] Integration test: PtyEvent pipeline (output + exit events)
- [ ] All tests pass (`cargo nextest run -p shux-pty`)
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes

---

## Commit Message

```
feat: add PTY manager with async read/write, resize, and lifecycle

- PtyHandle: spawn child via pty-process, async read/write, resize (TIOCSWINSZ)
- PtyManager: per-pane PTY tracking with HashMap<PaneId, PtyHandle>
- PtyEvent: Output, Exited, Restarted events via mpsc channel
- Async read loop with CancellationToken shutdown integration
- CWD tracking: Linux /proc/pid/cwd, macOS stub with fallback
- RestartPolicy: Never, OnFail, Always with should_restart() logic
- Integration tests with real PTY processes (echo, cat, true/false)
```

---

## Session Protocol

1. **Before starting:** Read `CLAUDE.md`, `docs/PRD.md` sections 4.2, 5.1, 15.2. Verify task 000 is complete (workspace builds, `shux-pty` crate exists). Verify task 001 is complete (CancellationToken is available from `shux-core::daemon`).
2. **During:** Start with `handle.rs` (Steps 1-6), then `manager.rs` (Steps 7-9), then wire up `lib.rs` (Step 10), update `Cargo.toml` (Step 11), then write tests (Step 12). Run `cargo check -p shux-pty` after each file.
3. **Key design decisions:**
   - The `PtyHandle` owns the PTY master and child process. When the read loop takes ownership, the manager no longer directly accesses the handle. This is a conscious design choice to avoid sharing mutable state. The alternative (keeping the handle in the manager and passing a reference to the read loop) would require `Arc<Mutex<PtyHandle>>`, adding lock contention.
   - The read buffer size (8 KiB) is a balance between throughput and latency. For bulk output (e.g., `find /`), larger buffers reduce syscall overhead. For interactive use (e.g., typing in vim), smaller buffers reduce latency. 8 KiB is a common default.
   - CWD tracking on macOS is left as a stub. The `proc_pidinfo` API is complex and requires unsafe C FFI. A production implementation should use the `libproc` crate or equivalent. This is acceptable for M0.
4. **After:** Run full verification suite. Update `docs/PROGRESS.md`. Update `CLAUDE.md` Learnings.
5. **Watch out for:**
   - `pty-process` 0.5 API changes: the crate's API for `Pty::new()`, `Command::spawn()`, and `Pty::resize()` may differ from older versions. Consult the docs at https://docs.rs/pty-process/0.5.
   - PTY read returns EAGAIN/EINTR on some platforms — these must be retried, not treated as fatal errors.
   - Integration tests that spawn shell processes may be flaky on CI due to shell startup time. Use generous timeouts (5s) and retry logic.
   - On macOS, `/proc` does not exist. The Linux-specific CWD tracking code must be behind `#[cfg(target_os = "linux")]`.
   - The `PtyHandle` must be `Send` for use in tokio tasks. Verify this compiles without issues.
   - The `cat` test (write input, read echoed output) is subtle: PTY echo means the input appears in the output twice (once from echo, once from cat). The test should check for the string appearing at least once, not exactly once.
