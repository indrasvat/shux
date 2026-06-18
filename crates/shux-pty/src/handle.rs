//! PtyHandle: per-pane PTY wrapper with async read/write/resize.

use std::fs::File;
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};

use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::pty::{Winsize, openpty};
use tokio::io::unix::AsyncFd;
use tracing::{debug, info};

const PANE_TERM_CANDIDATES: &[&str] = &["tmux-256color", "screen-256color", "xterm-256color"];
const DEFAULT_TERMINFO_DIRS: &[&str] = &[
    "/etc/terminfo",
    "/lib/terminfo",
    "/usr/share/terminfo",
    "/opt/homebrew/share/terminfo",
];

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

    #[error("child process error: {0}")]
    Child(std::io::Error),

    #[error("PTY handle closed")]
    Closed,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_terminfo_entry(root: &Path, term: &str) {
        let first = term.as_bytes()[0];
        let dir = root.join(format!("{first:x}"));
        std::fs::create_dir_all(&dir).unwrap();
        File::create(dir.join(term)).unwrap();
    }

    #[test]
    fn resolve_pane_term_prefers_tmux_when_available() {
        let root =
            std::env::temp_dir().join(format!("shux-pty-term-prefers-tmux-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        create_terminfo_entry(&root, "xterm-256color");
        create_terminfo_entry(&root, "screen-256color");
        create_terminfo_entry(&root, "tmux-256color");

        assert_eq!(
            resolve_pane_term_from_roots(std::slice::from_ref(&root)),
            "tmux-256color"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn resolve_pane_term_falls_back_to_screen_before_xterm() {
        let root = std::env::temp_dir().join(format!(
            "shux-pty-term-fallback-screen-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        create_terminfo_entry(&root, "xterm-256color");
        create_terminfo_entry(&root, "screen-256color");

        assert_eq!(
            resolve_pane_term_from_roots(std::slice::from_ref(&root)),
            "screen-256color"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn resolve_pane_term_uses_xterm_as_last_installed_candidate() {
        let root = std::env::temp_dir().join(format!(
            "shux-pty-term-fallback-xterm-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        create_terminfo_entry(&root, "xterm-256color");

        assert_eq!(
            resolve_pane_term_from_roots(std::slice::from_ref(&root)),
            "xterm-256color"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn linux_eio_is_treated_as_pty_eof() {
        #[cfg(target_os = "linux")]
        assert!(is_pty_eof_errno(nix::errno::Errno::EIO));

        #[cfg(not(target_os = "linux"))]
        assert!(!is_pty_eof_errno(nix::errno::Errno::EIO));
    }
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
            // Spawn as both login AND interactive. `-l` alone gets bash to
            // read `~/.bash_profile` but leaves `$-` without `i`, so any
            // interactive-only branch (the standard `[[ $- == *i* ]] &&
            // source ~/.bashrc` bridge, starship init, prompt frameworks)
            // gets skipped. `-l -i` runs both login and interactive
            // initialization paths — same flags iTerm2 uses by default.
            vec![shell, "-l".to_string(), "-i".to_string()]
        } else {
            self.command.clone()
        }
    }
}

/// A handle to a running PTY child process.
pub struct PtyHandle {
    pty: AsyncFd<OwnedFd>,
    child: Child,
    pid: u32,
    initial_cwd: PathBuf,
    size: PtySize,
}

fn nix_to_io(err: nix::Error) -> PtyError {
    PtyError::Open(std::io::Error::from(err))
}

fn set_nonblocking(fd: &OwnedFd) -> std::io::Result<()> {
    let flags = OFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFL)?);
    fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
    Ok(())
}

fn dup_stdio(fd: &OwnedFd) -> std::io::Result<Stdio> {
    let duped = nix::unistd::dup(fd)?;
    Ok(Stdio::from(File::from(duped)))
}

fn terminfo_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Some(path) = std::env::var_os("TERMINFO").filter(|value| !value.is_empty()) {
        roots.push(PathBuf::from(path));
    }

    if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        roots.push(PathBuf::from(home).join(".terminfo"));
    }

    if let Some(paths) = std::env::var_os("TERMINFO_DIRS").filter(|value| !value.is_empty()) {
        roots.extend(std::env::split_paths(&paths).filter(|path| !path.as_os_str().is_empty()));
    } else {
        roots.extend(DEFAULT_TERMINFO_DIRS.iter().map(PathBuf::from));
    }

    roots
}

fn terminfo_entry_exists(root: &Path, term: &str) -> bool {
    let Some(first) = term.as_bytes().first().copied() else {
        return false;
    };
    let first_char = char::from(first).to_string();
    let first_hex = format!("{first:x}");

    root.join(first_char).join(term).is_file() || root.join(first_hex).join(term).is_file()
}

fn resolve_pane_term_from_roots(roots: &[PathBuf]) -> &'static str {
    PANE_TERM_CANDIDATES
        .iter()
        .copied()
        .find(|term| {
            roots
                .iter()
                .any(|root| terminfo_entry_exists(root.as_path(), term))
        })
        .unwrap_or("xterm-256color")
}

fn resolve_pane_term() -> &'static str {
    resolve_pane_term_from_roots(&terminfo_roots())
}

fn is_pty_eof_errno(errno: nix::errno::Errno) -> bool {
    #[cfg(target_os = "linux")]
    {
        errno == nix::errno::Errno::EIO
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = errno;
        false
    }
}

fn drain_read(fd: std::os::fd::RawFd, buf: &mut [u8]) -> std::io::Result<usize> {
    // SAFETY: the fd is owned by self.pty and remains valid for the duration
    // of this synchronous nonblocking read.
    let fd = unsafe { BorrowedFd::borrow_raw(fd) };
    let mut total = 0usize;
    loop {
        match nix::unistd::read(fd, &mut buf[total..]) {
            Ok(0) => return Ok(total),
            Ok(n) => {
                total += n;
                if total == buf.len() {
                    return Ok(total);
                }
            }
            Err(nix::errno::Errno::EAGAIN) => {
                if total == 0 {
                    return Err(std::io::Error::from(std::io::ErrorKind::WouldBlock));
                }
                return Ok(total);
            }
            Err(nix::errno::Errno::EINTR) => continue,
            Err(e) if is_pty_eof_errno(e) => return Ok(total),
            Err(e) => return Err(std::io::Error::from(e)),
        }
    }
}

fn write_once(fd: std::os::fd::RawFd, buf: &[u8]) -> std::io::Result<usize> {
    // SAFETY: the fd is owned by self.pty and remains valid for the duration
    // of this synchronous nonblocking write.
    let fd = unsafe { BorrowedFd::borrow_raw(fd) };
    loop {
        match nix::unistd::write(fd, buf) {
            Ok(n) => return Ok(n),
            Err(nix::errno::Errno::EAGAIN) => {
                return Err(std::io::Error::from(std::io::ErrorKind::WouldBlock));
            }
            Err(nix::errno::Errno::EINTR) => continue,
            Err(e) => return Err(std::io::Error::from(e)),
        }
    }
}

impl PtyHandle {
    /// Spawn a new PTY child process.
    pub fn spawn(config: &PtyConfig) -> Result<Self, PtyError> {
        let winsize = Winsize {
            ws_row: config.size.rows,
            ws_col: config.size.cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty_pair = openpty(Some(&winsize), None).map_err(nix_to_io)?;
        set_nonblocking(&pty_pair.master).map_err(PtyError::Open)?;

        let cmd_parts = config.resolve_command();
        let program = &cmd_parts[0];
        let args = &cmd_parts[1..];

        let stdin = dup_stdio(&pty_pair.slave).map_err(PtyError::Open)?;
        let stdout = dup_stdio(&pty_pair.slave).map_err(PtyError::Open)?;
        let stderr = dup_stdio(&pty_pair.slave).map_err(PtyError::Open)?;

        let pane_term = resolve_pane_term();
        let mut cmd = Command::new(program);
        cmd.args(args)
            .current_dir(&config.cwd)
            .stdin(stdin)
            .stdout(stdout)
            .stderr(stderr)
            // shux is a terminal multiplexer, not a leaf emulator. Use the
            // same compatibility family as tmux/screen instead of xterm:
            // several CLIs probe xterm-like terminals with request/response
            // sequences and wait for a timeout when no emulator answers.
            // Prefer tmux over screen because its terminfo preserves richer
            // TUI capabilities such as italics, but fall back when the host
            // does not have that terminfo entry installed.
            .env("TERM", pane_term)
            // Pane children run inside an interactive PTY. If shux itself is
            // launched by an agent or wrapper with NO_COLOR=1, do not let that
            // degraded parent environment disable color inside every pane.
            // Explicit PtyConfig.env entries are applied below and can opt
            // back into NO_COLOR for a specific command.
            .env_remove("NO_COLOR")
            // Tell shells / prompts they're running inside shux, mirroring
            // tmux's TMUX env var. Users can guard config with
            // `[[ -n $SHUX ]] && ...` if they want shux-specific behavior.
            .env("SHUX", "1")
            // Hint truecolor support so colorful prompts (starship,
            // powerline) pick 24-bit codes by default.
            .env("COLORTERM", "truecolor")
            // Some BSD/macOS tools consult CLICOLOR even when TERM is good.
            .env("CLICOLOR", "1")
            // Claim TERM_PROGRAM so the parent emulator's value (e.g.
            // "WarpTerminal", "iTerm.app", "Apple_Terminal") does NOT
            // leak into the spawned shell. User rc files commonly branch
            // on TERM_PROGRAM (skipping starship under Warp, applying
            // iTerm-specific settings, etc.); inheriting the parent's
            // value silently turns those branches the wrong way inside a
            // shux pane. Setting our own marker is the same pattern tmux
            // uses (it sets TERM_PROGRAM=tmux).
            .env("TERM_PROGRAM", "shux")
            .env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));

        for (key, value) in &config.env {
            cmd.env(key, value);
        }

        let slave_fd = pty_pair.slave.as_raw_fd();
        // SAFETY: pre_exec runs in the child after fork and before exec. The
        // closure only calls async-signal-safe syscalls to create a session and
        // assign the slave PTY as the controlling terminal.
        unsafe {
            cmd.pre_exec(move || {
                nix::unistd::setsid().map_err(std::io::Error::from)?;
                #[cfg(any(target_os = "macos", target_os = "ios"))]
                let tiocsctty = nix::libc::TIOCSCTTY as nix::libc::c_ulong;
                #[cfg(not(any(target_os = "macos", target_os = "ios")))]
                let tiocsctty = nix::libc::TIOCSCTTY;
                let rc = nix::libc::ioctl(slave_fd, tiocsctty, 0);
                if rc == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let child = cmd.spawn().map_err(PtyError::Spawn)?;
        let pid = child.id();
        let pty = AsyncFd::new(pty_pair.master).map_err(PtyError::Open)?;

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
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            let mut guard = self.pty.readable_mut().await.map_err(PtyError::Read)?;
            match guard.try_io(|inner| drain_read(inner.get_ref().as_raw_fd(), buf)) {
                Ok(result) => return result.map_err(PtyError::Read),
                Err(_would_block) => continue,
            }
        }
    }

    /// Write bytes to the PTY (child's stdin).
    pub async fn write(&mut self, data: &[u8]) -> Result<(), PtyError> {
        let mut written = 0usize;
        while written < data.len() {
            let mut guard = self.pty.writable_mut().await.map_err(PtyError::Write)?;
            match guard.try_io(|inner| write_once(inner.get_ref().as_raw_fd(), &data[written..])) {
                Ok(Ok(0)) => {
                    return Err(PtyError::Write(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "PTY write returned zero",
                    )));
                }
                Ok(Ok(n)) => written += n,
                Ok(Err(e)) => return Err(PtyError::Write(e)),
                Err(_would_block) => continue,
            }
        }
        Ok(())
    }

    pub async fn write_str(&mut self, text: &str) -> Result<(), PtyError> {
        self.write(text.as_bytes()).await
    }

    pub async fn flush(&mut self) -> Result<(), PtyError> {
        Ok(())
    }

    /// Resize the PTY (sends TIOCSWINSZ/SIGWINCH to child).
    pub fn resize(&mut self, new_size: PtySize) -> Result<(), PtyError> {
        let winsize = Winsize {
            ws_row: new_size.rows,
            ws_col: new_size.cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let rc = unsafe {
            nix::libc::ioctl(
                self.pty.get_ref().as_raw_fd(),
                nix::libc::TIOCSWINSZ,
                &winsize,
            )
        };
        if rc == -1 {
            return Err(PtyError::Resize(std::io::Error::last_os_error()));
        }
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
        loop {
            if let Some(status) = self.try_wait()? {
                info!(pid = self.pid, ?status, "PTY child exited");
                return Ok(status);
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>, PtyError> {
        self.child.try_wait().map_err(PtyError::Child)
    }

    /// Ask the whole PTY process group to terminate.
    ///
    /// Spawned pane children are made session leaders, so the child PID is
    /// also the process group ID. Signalling the group matters
    /// for interactive shells: the foreground TUI may be a child of the shell,
    /// and killing only the shell can leave that foreground process alive.
    pub fn terminate(&mut self) -> Result<(), PtyError> {
        #[cfg(unix)]
        {
            if self
                .signal_process_group(nix::sys::signal::Signal::SIGHUP)
                .is_ok()
            {
                return Ok(());
            }
        }
        self.child.kill().map_err(PtyError::Child)
    }

    pub fn kill(&mut self) -> Result<(), PtyError> {
        #[cfg(unix)]
        {
            if self
                .signal_process_group(nix::sys::signal::Signal::SIGKILL)
                .is_ok()
            {
                return Ok(());
            }
        }
        self.child.kill().map_err(PtyError::Child)
    }

    #[cfg(unix)]
    fn signal_process_group(&self, signal: nix::sys::signal::Signal) -> Result<(), PtyError> {
        use nix::sys::signal::killpg;
        use nix::unistd::Pid;

        killpg(Pid::from_raw(self.pid as i32), signal)
            .map_err(|e| PtyError::Child(std::io::Error::from(e)))
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
            std::fs::read_link(path).ok()
        }

        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }
}
