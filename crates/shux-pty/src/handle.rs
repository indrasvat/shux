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

/// How often a read that is holding the slave fd open checks whether the child
/// has exited — one `waitpid(WNOHANG)` plus one `FIONREAD` per tick.
///
/// Tight while the pane is young and slack afterwards, because the two costs
/// pull in opposite directions and land on different panes. The tick bounds
/// how long after a child's exit the pane reaches EOF, and the panes that care
/// are the short-lived ones an agent runs and immediately captures — they are
/// gone inside the first seconds. A shell someone has had open all afternoon
/// cares about idle wakeups instead, and cannot tell 20ms from 200ms when it
/// finally exits.
const SLAVE_RELEASE_POLL_EAGER: std::time::Duration = std::time::Duration::from_millis(20);
const SLAVE_RELEASE_POLL_IDLE: std::time::Duration = std::time::Duration::from_millis(200);
const SLAVE_RELEASE_EAGER_FOR: std::time::Duration = std::time::Duration::from_secs(5);

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
    fn default_shell_argv_treats_a_blank_shell_as_unset() {
        assert_eq!(
            default_shell_argv(Some("/usr/bin/zsh")),
            vec!["/usr/bin/zsh", "-l", "-i"]
        );
        // The three ways `$SHELL` fails to name a shell all land on /bin/sh.
        for blank in [None, Some(""), Some("   "), Some("\t")] {
            assert_eq!(
                default_shell_argv(blank),
                vec!["/bin/sh", "-l", "-i"],
                "{blank:?} should fall back"
            );
        }
    }

    #[test]
    fn an_explicit_command_is_never_replaced_by_the_shell() {
        let cfg = PtyConfig::with_command(
            vec!["nvim".to_string(), "a.rs".to_string()],
            PathBuf::from("/tmp"),
        );
        assert_eq!(cfg.resolve_command(), vec!["nvim", "a.rs"]);
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

    // ── the drain loop at child exit (issue #162) ─────────────────────
    //
    // The sequences a pane's last read actually sees. These run the same code
    // on every host, which is the point: the macOS-only arm is what lost a
    // finished pane's output, and no Linux runner could reach it.

    /// `read` returning a scripted sequence, one call at a time.
    fn scripted(
        steps: Vec<nix::Result<&'static [u8]>>,
    ) -> impl FnMut(&mut [u8]) -> nix::Result<usize> {
        let mut steps = steps.into_iter();
        move |dst: &mut [u8]| match steps.next() {
            Some(Ok(bytes)) => {
                let n = bytes.len().min(dst.len());
                dst[..n].copy_from_slice(&bytes[..n]);
                Ok(n)
            }
            Some(Err(e)) => Err(e),
            None => Err(nix::errno::Errno::EAGAIN),
        }
    }

    #[test]
    fn eio_is_pty_eof_on_every_unix() {
        // Not just Linux: a macOS pane exit lands here too.
        assert!(is_pty_eof_errno(nix::errno::Errno::EIO));
        assert!(!is_pty_eof_errno(nix::errno::Errno::EAGAIN));
    }

    /// Kept under its original name so `check-test-inventory` sees no test
    /// disappear. It asserted a platform gate that no longer exists; what
    /// survives of it is the half that was always true, and the general
    /// contract now lives in `eio_is_pty_eof_on_every_unix`.
    #[test]
    fn linux_eio_is_treated_as_pty_eof() {
        assert!(is_pty_eof_errno(nix::errno::Errno::EIO));
    }

    #[test]
    fn the_last_bytes_before_eof_survive() {
        let mut buf = [0u8; 64];
        let n = drain_with(
            &mut buf,
            scripted(vec![Ok(b"TRUECOLOR\n"), Err(nix::errno::Errno::EIO)]),
        )
        .expect("EOF after a short read is not an error");
        assert_eq!(&buf[..n], b"TRUECOLOR\n");
    }

    #[test]
    fn a_read_error_never_eats_the_bytes_already_read() {
        // Any failing errno, not just the EOF one: the caller gets the data
        // now and the error on the next call.
        for errno in [nix::errno::Errno::ENXIO, nix::errno::Errno::EBADF] {
            let mut buf = [0u8; 64];
            let n = drain_with(&mut buf, scripted(vec![Ok(b"TRUECOLOR\n"), Err(errno)]))
                .unwrap_or_else(|e| panic!("{errno:?} after a short read dropped the bytes: {e}"));
            assert_eq!(&buf[..n], b"TRUECOLOR\n");
        }
    }

    #[test]
    fn an_error_with_nothing_buffered_is_still_an_error() {
        let mut buf = [0u8; 64];
        let err = drain_with(&mut buf, scripted(vec![Err(nix::errno::Errno::ENXIO)]))
            .expect_err("a bare read failure must surface");
        assert_eq!(err.raw_os_error(), Some(nix::errno::Errno::ENXIO as i32));

        // …and EOF with nothing buffered is a clean zero, not an error.
        assert_eq!(
            drain_with(&mut buf, scripted(vec![Err(nix::errno::Errno::EIO)])).unwrap(),
            0
        );
    }

    #[test]
    fn a_full_buffer_stops_the_drain() {
        let mut buf = [0u8; 4];
        assert_eq!(
            drain_with(&mut buf, scripted(vec![Ok(b"ab"), Ok(b"cd"), Ok(b"ef")])).unwrap(),
            4
        );
        assert_eq!(&buf, b"abcd");
    }

    #[test]
    fn nothing_readable_yet_is_would_block() {
        let mut buf = [0u8; 64];
        let err = drain_with(&mut buf, scripted(vec![Err(nix::errno::Errno::EAGAIN)]))
            .expect_err("an empty nonblocking read must not look like EOF");
        assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);
    }
}

/// Variables by which a terminal emulator or host advertises *itself*. shux is
/// the terminal a pane talks to, so inside a pane these name the wrong one.
///
/// Exact names; vendor families live in [`OUTER_TERMINAL_IDENTITY_PREFIXES`].
/// `TERM_PROGRAM`/`TERM_PROGRAM_VERSION` are absent deliberately: shux
/// overwrites those instead (see the `.env` call that sets them).
///
/// `COLORFGBG` is absent for a different reason: removing it is WORSE than
/// leaking it. vim does not issue OSC 11 under `TERM=tmux-256color`, so with
/// `COLORFGBG` gone it has no polarity signal and falls back to
/// `background=light` -- measured, in a dark pane. It needs the
/// `TERM_PROGRAM` treatment (set from shux's own theme) rather than removal,
/// and that needs the theme plumbed into this crate.
pub const OUTER_TERMINAL_IDENTITY_VARS: &[&str] = &[
    "TMUX",
    "TMUX_PANE",
    "STY",
    "ZELLIJ",
    // Set by terminal-browser's tty7 backend (zenbu-labs/terminal-browser) --
    // the app whose blank pane prompted this scrub.
    "TTY7_PANE",
    // `NVIM` names the OUTER nvim's socket, so `nvim` in a pane opens the file
    // in the outer editor.
    "NVIM",
    "NVIM_LISTEN_ADDRESS",
    "INSIDE_EMACS",
    // NOT locale categories despite the prefix: iTerm2 uses `LC_*` so ssh's
    // `SendEnv LC_*` forwards them, and POSIX ignores unknown `LC_*` names.
    "ITERM_SESSION_ID",
    "ITERM_PROFILE",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "TERM_SESSION_ID",
    "VTE_VERSION",
    "GNOME_TERMINAL_SCREEN",
    "GNOME_TERMINAL_SERVICE",
    "XTERM_VERSION",
    "TERMINAL_EMULATOR",
    "TERMINOLOGY",
    "CONTOUR_PROFILE",
    "TILIX_ID",
    "TERMINATOR_UUID",
    "TERMINATOR_DBUS_NAME",
    "TERMINATOR_DBUS_PATH",
    "WT_SESSION",
    "WT_PROFILE_ID",
    // Describe geometry or a window the pane is not drawing into. ncurses
    // prefers `COLUMNS`/`LINES` over the pty ioctl, so a stale pair makes every
    // `tput cols` script render at the OUTER terminal's width -- measured 203
    // in an 80-column pane. `COLORFGBG` is deliberately NOT here -- see this const's own doc.
    "WINDOWID",
    "COLUMNS",
    "LINES",
    // Drive VS Code's OSC 633 integration, which shux's VT swallows anyway. Do
    // NOT make this a `VSCODE_` prefix: that eats `VSCODE_IPC_HOOK_CLI` (breaks
    // `code file` from a pane) and the `VSCODE_GIT_ASKPASS_*` trio.
    "VSCODE_INJECTION",
    "VSCODE_SHELL_INTEGRATION",
];

/// Vendor prefixes whose whole family names the outer terminal -- prefixes, so
/// a variable an emulator adds later is still caught. Every entry is a full
/// vendor word ending in `_`; anything shorter claims more namespace than it
/// can justify and belongs in [`OUTER_TERMINAL_IDENTITY_VARS`] instead.
pub const OUTER_TERMINAL_IDENTITY_PREFIXES: &[&str] = &[
    "KITTY_",
    "GHOSTTY_",
    "WEZTERM_",
    "ALACRITTY_",
    "KONSOLE_",
    "ZELLIJ_",
    "WARP_",
];

/// A vendor namespace holds the user's configuration too, and scrubbing that is
/// the same silent misbehaviour: `ZELLIJ_CONFIG_DIR` stripped from a pane makes
/// a nested zellij load defaults. A rule rather than a third list, so it covers
/// vendor knobs nobody has written down yet.
///
/// `rest` is the key with its vendor prefix already stripped, and the word must
/// be a whole segment of it -- `KITTY_CONFIG_DIR` and a bare `KITTY_CONFIG` are
/// the user's, `KITTY_CONFIGURED_ID` is identity that merely starts the same
/// way. Anchoring only one of the two words is how that last one got exempted.
fn is_user_config_in_vendor_namespace(rest: &[u8]) -> bool {
    [b"CONFIG".as_slice(), b"AUTO".as_slice()]
        .iter()
        .any(|word| leads_with_segment(rest, word))
}

/// `word` is `rest`'s first `_`-delimited segment, or all of it.
fn leads_with_segment(rest: &[u8], word: &[u8]) -> bool {
    rest.strip_prefix(word)
        .is_some_and(|tail| tail.is_empty() || tail.starts_with(b"_"))
}

/// Configuration for spawning a PTY child process.
#[derive(Debug, Clone)]
pub struct PtyConfig {
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub size: PtySize,
    /// Deny-by-default inheritance (task 081 D4): when true, `spawn` clears the
    /// inherited environment BEFORE applying the PTY defaults + `env`, so the child
    /// sees ONLY the deterministic plan. Default `false` = byte-identical prior
    /// behaviour (the scratch gate runner is the only caller that sets it).
    pub env_clear: bool,
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
            env_clear: false,
        }
    }

    pub fn with_command(command: Vec<String>, cwd: PathBuf) -> Self {
        Self {
            command,
            cwd,
            env: Vec::new(),
            size: PtySize::default(),
            env_clear: false,
        }
    }

    fn resolve_command(&self) -> Vec<String> {
        if self.command.is_empty() {
            default_shell_argv(std::env::var("SHELL").ok().as_deref())
        } else {
            self.command.clone()
        }
    }
}

/// The argv for a pane with no explicit command.
///
/// Spawned as both login AND interactive. `-l` alone gets bash to read
/// `~/.bash_profile` but leaves `$-` without `i`, so any interactive-only
/// branch (the standard `[[ $- == *i* ]] && source ~/.bashrc` bridge, starship
/// init, prompt frameworks) gets skipped. `-l -i` runs both login and
/// interactive initialization paths — the same flags iTerm2 uses by default.
///
/// A **blank** `$SHELL` is treated as unset. `env::var` returns `Ok("")` for a
/// set-but-empty variable, so an `unwrap_or_else` fallback never fires and the
/// pane execs the empty program name: it dies instantly with an error naming
/// neither the pane nor the cause. This is the same rule the `command` RPC
/// parameter resolves its shell by, so the two cannot disagree about which
/// shell a pane gets (issue #125 follow-up).
fn default_shell_argv(shell_env: Option<&str>) -> Vec<String> {
    let shell = shell_env
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("/bin/sh");
    vec![shell.to_string(), "-l".to_string(), "-i".to_string()]
}

/// A handle to a running PTY child process.
pub struct PtyHandle {
    pty: AsyncFd<OwnedFd>,
    /// The parent's own slave fd, held open for as long as the child lives.
    ///
    /// A tty discards whatever is still queued when its **last** slave fd
    /// closes, so a child that writes once and exits can have its final bytes
    /// destroyed before the reader has run at all — the pane then reports its
    /// exit status against an empty grid (issue #162). Holding one open means
    /// the child's exit is not the last close. The pane task drops it via
    /// [`PtyHandle::release_slave`] once the child is reaped *and* the master
    /// has nothing queued, which is what lets the read reach EOF.
    slave: Option<OwnedFd>,
    child: Child,
    pid: u32,
    /// When the child was spawned — only used to slacken the exit poll above.
    spawned_at: std::time::Instant,
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

/// `EIO` on a PTY master read is the master's EOF: the last slave fd closed,
/// i.e. the child and everything holding its terminal are gone.
///
/// Every unix reports it that way, not just Linux. Gating it to Linux left
/// macOS taking the read-*error* path on every pane exit, which — with the
/// pre-#162 drain below — threw away the bytes the same call had already read.
/// A pane that printed and exited then reported its exit status with an empty
/// grid, so an agent's `PaneExited` → `pane.capture` loop captured nothing.
fn is_pty_eof_errno(errno: nix::errno::Errno) -> bool {
    errno == nix::errno::Errno::EIO
}

/// `si_pid`, which libc exposes as a plain field on BSD/macOS and behind an
/// accessor on Linux. Zero means `waitid` found nothing waitable.
fn siginfo_pid(info: &nix::libc::siginfo_t) -> nix::libc::pid_t {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: reading the pid of a siginfo `waitid` just filled in (or of
        // the zeroed struct it left untouched) is always valid.
        unsafe { info.si_pid() }
    }
    #[cfg(not(target_os = "linux"))]
    {
        info.si_pid
    }
}

fn drain_read(fd: std::os::fd::RawFd, buf: &mut [u8]) -> std::io::Result<usize> {
    // SAFETY: the fd is owned by self.pty and remains valid for the duration
    // of this synchronous nonblocking read.
    let fd = unsafe { BorrowedFd::borrow_raw(fd) };
    drain_with(buf, |dst| nix::unistd::read(fd, dst))
}

/// The drain loop, over an injectable read so the exit-time errno sequences
/// can be tested on any host (issue #162).
///
/// Bytes already in `buf` are never dropped: an error that arrives after a
/// short read is reported on the *next* call, once the caller has the data.
/// Read failures are sticky — the fd stays broken — so nothing is lost by
/// deferring one, while a discarded read is gone for good.
fn drain_with<F>(buf: &mut [u8], mut read: F) -> std::io::Result<usize>
where
    F: FnMut(&mut [u8]) -> nix::Result<usize>,
{
    let mut total = 0usize;
    loop {
        match read(&mut buf[total..]) {
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
            Err(e) => {
                if total > 0 {
                    return Ok(total);
                }
                return Err(std::io::Error::from(e));
            }
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
    ///
    /// Reads the process environment to scrub outer-terminal identity (see
    /// [`OUTER_TERMINAL_IDENTITY_VARS`]), so it must not run concurrently with
    /// `std::env::set_var`.
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
        // Deny-by-default env (task 081 D4): clear inherited vars FIRST so the child
        // sees only the PTY defaults below + the caller's explicit `env` plan. A
        // deterministic `PATH` MUST be in `env` for relative-program resolution.
        if config.env_clear {
            cmd.env_clear();
        }
        // Ordering is load-bearing: BEFORE the `.env()` calls below, so a name
        // that is both scrubbed and set by shux keeps shux's value, and before
        // `config.env`, so a caller can put one back.
        for key in OUTER_TERMINAL_IDENTITY_VARS {
            cmd.env_remove(key);
        }
        // screen's bare `WINDOW` is only screen's when `STY` is also set, and
        // set to something: screen always writes a session id, so an empty
        // `STY` proves nothing and the user's own `WINDOW` must survive it.
        if std::env::var_os("STY").is_some_and(|sty| !sty.is_empty()) {
            cmd.env_remove("WINDOW");
        }
        // Bytes, not `to_str`: a key that is not valid UTF-8 would skip silently.
        for (key, _) in std::env::vars_os() {
            let bytes = key.as_encoded_bytes();
            let Some(rest) = OUTER_TERMINAL_IDENTITY_PREFIXES
                .iter()
                .find_map(|prefix| bytes.strip_prefix(prefix.as_bytes()))
            else {
                continue;
            };
            if !is_user_config_in_vendor_namespace(rest) {
                cmd.env_remove(&key);
            }
        }

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
            // Kept, not dropped: see the field's docs (issue #162).
            slave: Some(pty_pair.slave),
            child,
            pid,
            spawned_at: std::time::Instant::now(),
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
    ///
    /// `Ok(0)` is EOF, as it always was. Reaching it now includes dropping the
    /// slave fd this handle holds on the child's behalf (issue #162) — done
    /// here, inside the read, so every caller keeps the plain read-until-EOF
    /// contract and none has to know about the fd at all.
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize, PtyError> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if self.slave.is_some() {
                // While we hold a slave open the master cannot report EOF, so
                // the wait for readability is raced against a poll for the
                // child's exit. Biased: queued output is always drained first,
                // and the release only happens with the queue empty, because
                // on BSD/macOS that close discards whatever is still in it.
                let poll = self.slave_release_poll();
                let exit_tick = tokio::select! {
                    biased;

                    guard = self.pty.readable_mut() => {
                        let mut guard = guard.map_err(PtyError::Read)?;
                        match guard.try_io(|inner| drain_read(inner.get_ref().as_raw_fd(), buf)) {
                            Ok(result) => return result.map_err(PtyError::Read),
                            Err(_would_block) => false,
                        }
                    }
                    _ = tokio::time::sleep(poll) => true,
                };
                if exit_tick
                    && self.child_exited_unreaped()
                    && self.pending_input_bytes().unwrap_or(0) == 0
                {
                    self.release_slave();
                }
                continue;
            }

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

    /// Bytes the master can read right now.
    ///
    /// Used to decide when releasing the retained slave fd is safe: at zero
    /// there is nothing left for the close to discard.
    fn pending_input_bytes(&self) -> Result<usize, PtyError> {
        let mut n: nix::libc::c_int = 0;
        // SAFETY: `self.pty` owns the fd for the duration of the call, and
        // FIONREAD writes a single c_int through the pointer we pass.
        let rc = unsafe {
            nix::libc::ioctl(
                self.pty.get_ref().as_raw_fd(),
                nix::libc::FIONREAD,
                &mut n as *mut nix::libc::c_int,
            )
        };
        if rc == -1 {
            return Err(PtyError::Read(std::io::Error::last_os_error()));
        }
        Ok(n.max(0) as usize)
    }

    /// Has the child exited? Answered **without reaping it**.
    ///
    /// `Child::try_wait` would reap, and reaping frees the pid — which is also
    /// this pane's process *group* id, since the child is a session leader.
    /// A pane whose child exits while a descendant keeps the tty open is still
    /// a live group that teardown has to be able to signal, and a freed pgid is
    /// one an unrelated process can be handed. `WNOWAIT` leaves the zombie in
    /// place, so the group stays allocated and the real reap stays exactly
    /// where it always was: `wait()`, after the read loop ends.
    fn child_exited_unreaped(&self) -> bool {
        // libc directly, because nix's `waitid` wrapper is not compiled for
        // Apple targets — and macOS is the platform this whole fix exists for.
        // POSIX idiom: zero the struct first, since a `WNOHANG` call that finds
        // nothing waitable returns 0 and leaves `si_pid` untouched.
        let mut info: nix::libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a live, correctly-typed `siginfo_t` for the whole
        // call, and it is the only thing `waitid` writes through the pointer.
        let rc = unsafe {
            nix::libc::waitid(
                nix::libc::P_PID,
                self.pid as nix::libc::id_t,
                &mut info,
                nix::libc::WEXITED | nix::libc::WNOHANG | nix::libc::WNOWAIT,
            )
        };
        if rc == -1 {
            // ECHILD: not ours to wait on any more, so it is certainly gone.
            let e = std::io::Error::last_os_error();
            debug!(pid = self.pid, error = %e, "PTY child waitid failed");
            return true;
        }
        siginfo_pid(&info) != 0
    }

    /// The current exit-poll interval — see the constants.
    fn slave_release_poll(&self) -> std::time::Duration {
        if self.spawned_at.elapsed() < SLAVE_RELEASE_EAGER_FOR {
            SLAVE_RELEASE_POLL_EAGER
        } else {
            SLAVE_RELEASE_POLL_IDLE
        }
    }

    /// Drop the parent's slave fd, so a master read can reach EOF.
    ///
    /// Only safe once the child is reaped and [`Self::pending_input_bytes`] is
    /// zero: on BSD/macOS this close is what discards anything still queued.
    /// Idempotent.
    fn release_slave(&mut self) {
        self.slave.take();
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
