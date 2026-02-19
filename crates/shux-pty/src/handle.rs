//! PtyHandle: per-pane PTY wrapper with async read/write/resize.

use std::path::PathBuf;
use std::process::ExitStatus;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, info};

/// Errors from PTY operations.
#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    #[error("failed to open PTY: {0}")]
    Open(pty_process::Error),

    #[error("failed to spawn child process: {0}")]
    Spawn(pty_process::Error),

    #[error("failed to read from PTY: {0}")]
    Read(std::io::Error),

    #[error("failed to write to PTY: {0}")]
    Write(std::io::Error),

    #[error("failed to resize PTY: {0}")]
    Resize(pty_process::Error),

    #[error("child process error: {0}")]
    Child(std::io::Error),

    #[error("PTY handle closed")]
    Closed,
}

/// Configuration for spawning a PTY child process.
#[derive(Debug, Clone)]
pub struct PtyConfig {
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
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
    pub fn default_shell(cwd: PathBuf) -> Self {
        Self {
            command: Vec::new(),
            cwd,
            env: Vec::new(),
            size: PtySize::default(),
        }
    }

    pub fn with_command(command: Vec<String>, cwd: PathBuf) -> Self {
        Self {
            command,
            cwd,
            env: Vec::new(),
            size: PtySize::default(),
        }
    }

    fn resolve_command(&self) -> Vec<String> {
        if self.command.is_empty() {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
            vec![shell, "-l".to_string()]
        } else {
            self.command.clone()
        }
    }
}

/// A handle to a running PTY child process.
pub struct PtyHandle {
    pty: pty_process::Pty,
    child: tokio::process::Child,
    pid: u32,
    initial_cwd: PathBuf,
    size: PtySize,
}

impl PtyHandle {
    /// Spawn a new PTY child process.
    pub fn spawn(config: &PtyConfig) -> Result<Self, PtyError> {
        let (pty, pts) = pty_process::open().map_err(PtyError::Open)?;

        pty.resize(pty_process::Size::new(config.size.rows, config.size.cols))
            .map_err(PtyError::Resize)?;

        let cmd_parts = config.resolve_command();
        let program = &cmd_parts[0];
        let args = &cmd_parts[1..];

        let mut cmd = pty_process::Command::new(program)
            .args(args)
            .current_dir(&config.cwd)
            .env("TERM", "xterm-256color");

        for (key, value) in &config.env {
            cmd = cmd.env(key, value);
        }

        let child = cmd.spawn(pts).map_err(PtyError::Spawn)?;
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

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn initial_cwd(&self) -> &PathBuf {
        &self.initial_cwd
    }

    pub fn size(&self) -> PtySize {
        self.size
    }

    /// Read bytes from the PTY (child's stdout/stderr).
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize, PtyError> {
        self.pty.read(buf).await.map_err(PtyError::Read)
    }

    /// Write bytes to the PTY (child's stdin).
    pub async fn write(&mut self, data: &[u8]) -> Result<(), PtyError> {
        self.pty.write_all(data).await.map_err(PtyError::Write)
    }

    pub async fn write_str(&mut self, text: &str) -> Result<(), PtyError> {
        self.write(text.as_bytes()).await
    }

    pub async fn flush(&mut self) -> Result<(), PtyError> {
        self.pty.flush().await.map_err(PtyError::Write)
    }

    /// Resize the PTY (sends TIOCSWINSZ/SIGWINCH to child).
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

    /// Wait for the child process to exit.
    pub async fn wait(&mut self) -> Result<ExitStatus, PtyError> {
        let status = self.child.wait().await.map_err(PtyError::Child)?;
        info!(pid = self.pid, ?status, "PTY child exited");
        Ok(status)
    }

    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>, PtyError> {
        self.child.try_wait().map_err(PtyError::Child)
    }

    pub fn kill(&mut self) -> Result<(), PtyError> {
        // start_kill is non-async kill on tokio::process::Child
        self.child.start_kill().map_err(PtyError::Child)
    }

    /// Get the current working directory of the child process.
    pub fn current_cwd(&self) -> PathBuf {
        self.try_current_cwd()
            .unwrap_or_else(|| self.initial_cwd.clone())
    }

    fn try_current_cwd(&self) -> Option<PathBuf> {
        #[cfg(target_os = "linux")]
        {
            let path = format!("/proc/{}/cwd", self.pid);
            return std::fs::read_link(path).ok();
        }

        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }
}
