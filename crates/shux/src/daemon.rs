//! OS-level daemon lifecycle: paths, PID file, daemonization, and signal handling.
//!
//! This module handles the Unix-specific aspects of the daemon — double-fork
//! daemonization, runtime directory management, PID files, and signal handlers.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

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
    #[error("failed to lock {}: {}", .0.display(), .1)]
    Lock(PathBuf, io::Error),
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

/// Full path to the PID file for the daemon serving `socket`.
///
/// Keyed to the socket because the socket is overridable while the runtime dir is
/// not: two daemons in one runtime dir used to share `shux.pid`, and the second
/// overwrote the first, leaving it unreachable. The default socket keeps exactly
/// `$RUNTIME_DIR/shux.pid`, so the path documented on `--socket` stays true.
///
/// It stays inside the runtime dir rather than beside a user-supplied socket:
/// that directory is proven ours and 0700, and a pidfile somewhere world-writable
/// is a pid an attacker can choose. The hash is hand-rolled because
/// `DefaultHasher` is explicitly unstable across releases, and a differently
/// versioned client has to find this file.
pub fn pid_file_path_for(socket: &Path) -> Result<PathBuf, DaemonError> {
    pid_file_for_spelling(&socket_key(socket))
}

fn pid_file_for_spelling(socket: &Path) -> Result<PathBuf, DaemonError> {
    Ok(runtime_dir()?.join(format!("shux{}.pid", suffix_for_spelling(socket))))
}

/// Empty for the default socket, `-<hash>` otherwise.
fn suffix_for_spelling(socket: &Path) -> String {
    if socket_path().is_ok_and(|default| socket_key(&default) == socket) {
        return String::new();
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in socket.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("-{hash:016x}")
}

/// One spelling for every path naming the same socket, so `a//s.sock`,
/// `a/./s.sock` and a symlinked parent share one pidfile and one pair of locks.
///
/// The deepest existing ancestor is canonicalised and the rest normalised
/// lexically: the socket and its directory may not exist yet, and the key must
/// not change when they come to.
pub fn socket_key(socket: &Path) -> PathBuf {
    use std::path::Component;
    let mut rest = Vec::new();
    let mut base = socket;
    let mut key = loop {
        let dir = if base.as_os_str().is_empty() {
            Path::new(".")
        } else {
            base
        };
        if let Ok(real) = dir.canonicalize() {
            break real;
        }
        match (base.parent(), base.components().next_back()) {
            (Some(parent), Some(last)) => {
                rest.push(last);
                base = parent;
            }
            _ => return socket.to_path_buf(),
        }
    };
    for part in rest.into_iter().rev() {
        match part {
            Component::ParentDir => {
                key.pop();
            }
            Component::Normal(name) => key.push(name),
            _ => {}
        }
    }
    key
}

/// The path to bind or connect at for `socket`: its [`socket_key`] when that
/// fits a socket address, otherwise the spelling made absolute without
/// resolving anything; `..` is kept, since after a symlink only the kernel
/// knows where it leads. Identity -- pidfile, locks -- stays on the key; only the
/// syscall needs a path the kernel accepts, and resolving a symlinked runtime
/// dir (`/var` → `/private/var` on macOS) can push it past the limit.
pub fn socket_endpoint(socket: &Path) -> PathBuf {
    let key = socket_key(socket);
    if nix::sys::socket::UnixAddr::new(&key).is_ok() {
        return key;
    }
    let mut lexical = if socket.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().unwrap_or_default()
    };
    for part in socket.components() {
        if part != std::path::Component::CurDir {
            lexical.push(part);
        }
    }
    if nix::sys::socket::UnixAddr::new(&lexical).is_ok() {
        lexical
    } else {
        key
    }
}

/// The socket as the user spelled it, for [`pid_file_candidates`]: the CLI
/// passes the key everywhere else.
static TYPED_SOCKET: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Record the `--socket` / `SHUX_SOCKET` spelling this process was given.
pub fn remember_typed_socket(typed: &Path) {
    let _ = TYPED_SOCKET.set(typed.to_path_buf());
}

/// The spelling [`remember_typed_socket`] recorded, if any.
pub fn typed_socket() -> Option<&'static Path> {
    TYPED_SOCKET.get().map(PathBuf::as_path)
}

/// Pidfile paths to consult for `socket`, most specific first.
///
/// The others are where earlier versions wrote: a hash of the socket as typed,
/// and before that `$RUNTIME_DIR/shux.pid` whatever socket was served. Skipping
/// them would orphan the daemon being replaced at every upgrade, and such a
/// daemon holds no instance lock to be found by. Safe because the caller
/// identity-checks the pid it finds against the socket it serves.
pub fn pid_file_candidates(socket: &Path) -> Result<Vec<PathBuf>, DaemonError> {
    let mut candidates = vec![pid_file_path_for(socket)?];
    let typed = TYPED_SOCKET
        .get()
        .filter(|typed| socket_key(typed) == socket_key(socket));
    let mut older = vec![pid_file_for_spelling(socket)?];
    if let Some(typed) = typed {
        older.push(pid_file_for_spelling(typed)?);
    }
    older.push(runtime_dir()?.join("shux.pid"));
    for older in older {
        if !candidates.contains(&older) {
            candidates.push(older);
        }
    }
    Ok(candidates)
}

/// Lock file a live daemon for `socket` holds for its whole lifetime.
///
/// Never unlinked: a lock on an unlinked inode excludes nobody, so a daemon
/// removing it on shutdown would let a successor lock a fresh file while the
/// predecessor still owns the socket.
pub fn instance_lock_path_for(socket: &Path) -> Result<PathBuf, DaemonError> {
    Ok(pid_file_path_for(socket)?.with_extension("lock"))
}

/// Lock file clients hold while checking for, replacing or starting the daemon
/// for `socket`. Distinct from the instance lock, which the daemon being
/// started has to be able to take while its starter still holds this one.
pub fn start_lock_path_for(socket: &Path) -> Result<PathBuf, DaemonError> {
    Ok(pid_file_path_for(socket)?.with_extension("start.lock"))
}

/// Take an exclusive `flock` on `path` without blocking.
///
/// `Ok(None)` means another process holds it. The lock lives as long as the
/// returned file, and std opens it `O_CLOEXEC`, so pane children never inherit
/// it and outlive the daemon holding it.
pub fn try_lock_file(path: &Path) -> Result<Option<fs::File>, DaemonError> {
    let mut opts = fs::OpenOptions::new();
    opts.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(nix::libc::O_NOFOLLOW);
    }
    let fail = |e| DaemonError::Lock(path.to_path_buf(), e);
    let file = opts.open(path).map_err(fail)?;
    // A FIFO or device opens and locks fine, then fails the daemon's pid write.
    if !file.metadata().map_err(fail)?.is_file() {
        return Err(fail(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        )));
    }
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(e)) => Err(fail(e)),
    }
}

/// Read a pid from an explicit pidfile path.
pub fn read_pid_at(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Full path to the Unix domain socket: `$RUNTIME_DIR/shux.sock`
pub fn socket_path() -> Result<PathBuf, DaemonError> {
    Ok(runtime_dir()?.join("shux.sock"))
}

/// The attach-session socket of the daemon serving `socket`: `attach.sock` for
/// the default socket, keyed like the pidfile otherwise, so daemons on other
/// sockets in the same runtime dir do not take it over. Distinct from the
/// JSON-RPC socket so the attach loop doesn't have to multiplex protocols on a
/// single connection.
pub fn attach_socket_path_for(socket: &Path) -> Result<PathBuf, DaemonError> {
    let suffix = suffix_for_spelling(&socket_key(socket));
    Ok(runtime_dir()?.join(format!("attach{suffix}.sock")))
}

/// Ensure the runtime directory exists with mode 0700.
///
/// Security (CWE-59): the runtime dir is predictable, and in the fallback
/// (`$TMPDIR/shux-$UID/`, used when `XDG_RUNTIME_DIR` is unset) it lives in
/// shared `/tmp`. A local attacker could pre-plant the final component as a
/// symlink to a directory they control; a naive `chmod(0700)` would *follow*
/// it (tightening someone else's dir) and the daemon would then write its
/// socket, pidfile, and materialised segment configs into an attacker-chosen
/// location — defeating the "daemon's own private dir" guarantee the statusbar
/// fix relies on. We therefore open the final component with `O_NOFOLLOW`
/// (refusing a symlink outright) and `fchmod` the resulting handle, which acts
/// on the real directory with no path re-resolution (no TOCTOU window). We also
/// require the opened directory to be **owned by us** — a symlink is not the
/// only squat: a local user can pre-create the predictable fallback path as a
/// real directory they own, which `O_NOFOLLOW` accepts and `fchmod` cannot
/// re-own. Refusing a foreign-owned dir fails closed rather than running out of
/// attacker-controlled storage.
pub fn ensure_runtime_dir() -> Result<PathBuf, DaemonError> {
    let dir = runtime_dir()?;
    // `create_dir_all` treats a pre-existing symlink-to-dir as the dir and
    // succeeds, so the no-follow check below (not this call) is what refuses it.
    fs::create_dir_all(&dir).map_err(DaemonError::CreateDir)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        let handle = fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
            .open(&dir)
            .map_err(DaemonError::CreateDir)?;
        // Require the directory to be owned by us. `O_NOFOLLOW` refused a
        // symlink, but a local user can pre-create the predictable fallback path
        // (`$TMPDIR/shux-$UID`, in shared /tmp) as a real directory THEY own;
        // opening it succeeds and `fchmod(0700)` tightens the mode but CANNOT
        // change the owner. A daemon (esp. as root) would then run its socket,
        // pidfile and materialised segment configs out of an attacker-owned
        // directory whose owner can unlink/replace those entries by pathname —
        // e.g. swap `segment-<pid>-<idx>.toml` and feed root's starship an
        // attacker-controlled config. `metadata()` here is an `fstat` on the
        // open handle (no path re-resolution), so the owner we check is the
        // directory we will actually write into.
        let owner = handle.metadata().map_err(DaemonError::CreateDir)?.uid();
        let me = nix::unistd::geteuid().as_raw();
        if owner != me {
            return Err(DaemonError::CreateDir(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "runtime directory {} is owned by uid {owner}, not {me}; refusing to use it",
                    dir.display()
                ),
            )));
        }
        handle
            .set_permissions(fs::Permissions::from_mode(0o700))
            .map_err(DaemonError::CreateDir)?;
    }

    Ok(dir)
}

/// Write the current process PID to the PID file.
///
/// Opened `O_NOFOLLOW` (CWE-59): even though `ensure_runtime_dir` now guarantees
/// a real 0700 directory, refusing to write through a symlink at the pidfile
/// path is cheap defense-in-depth against an arbitrary-file clobber. A stale
/// regular pidfile from a previous run is still overwritten (create + truncate).
pub fn write_pid_file(socket: &Path) -> Result<(), DaemonError> {
    use std::io::Write;
    let path = pid_file_path_for(socket)?;
    let pid = std::process::id();
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC);
    }
    let mut file = opts.open(&path).map_err(DaemonError::PidFile)?;
    file.write_all(pid.to_string().as_bytes())
        .map_err(DaemonError::PidFile)?;
    Ok(())
}

/// Remove the PID file on shutdown, if it still names this process: a daemon of
/// another build may have started on the socket since and written its own.
pub fn remove_own_pid_file_for(socket: &Path) -> Result<(), DaemonError> {
    let path = pid_file_path_for(socket)?;
    if read_pid_at(&path) != Some(std::process::id()) {
        return Ok(());
    }
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(DaemonError::RemovePidFile(e)),
    }
}

/// Whether this daemon still holds the instance lock at its path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Custody {
    Held,
    /// Another process holds the lock now: it serves the socket, not us.
    Lost,
    /// Could not tell, e.g. the runtime dir cannot be recreated safely.
    Unknown,
}

/// The instance lock a daemon holds for its lifetime.
///
/// A lock on an unlinked file excludes nobody: when the runtime dir or the lock
/// file is deleted, a client sees no lock and starts a second daemon. So the
/// path is re-checked, and a missing file is recreated, locked and repopulated.
pub struct InstanceLock {
    socket: PathBuf,
    file: std::sync::Mutex<fs::File>,
}

impl InstanceLock {
    pub fn check(&self) -> Custody {
        use std::os::unix::fs::MetadataExt;
        let Ok(path) = instance_lock_path_for(&self.socket) else {
            return Custody::Unknown;
        };
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let id = |m: fs::Metadata| (m.dev(), m.ino());
        if let (Ok(at_path), Ok(held)) = (fs::symlink_metadata(&path), file.metadata())
            && id(at_path) == id(held)
        {
            return Custody::Held;
        }
        if ensure_runtime_dir().is_err() {
            return Custody::Unknown;
        }
        match try_lock_file(&path) {
            Ok(Some(mut fresh)) => {
                use std::io::Write;
                if write!(fresh, "{}", std::process::id()).is_err() {
                    return Custody::Unknown;
                }
                *file = fresh;
                let _ = write_pid_file(&self.socket);
                tracing::warn!(lock = %path.display(), "instance lock was deleted; took it again");
                Custody::Held
            }
            Ok(None) => Custody::Lost,
            Err(_) => Custody::Unknown,
        }
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
/// 4. Redirect stdin/stdout/stderr to /dev/null, and close every other
///    descriptor the starter left open
/// 5. Take the instance lock and record our pid in it
/// 6. Write PID file
///
/// Returns the instance lock in the daemon, and `None` in every other process:
/// the original parent, and a daemon that found one already running for
/// `socket`. Dropping the lock releases the socket to a successor.
pub fn daemonize(socket: &Path) -> Result<Option<InstanceLock>, DaemonError> {
    use nix::unistd::{ForkResult, fork, setsid};

    // SAFETY: This is called before any tokio runtime is created, so the process
    // is single-threaded. fork() is safe in single-threaded processes.
    match unsafe { fork() }.map_err(DaemonError::Fork)? {
        ForkResult::Parent { .. } => {
            return Ok(None);
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
    close_inherited_descriptors();

    ensure_runtime_dir()?;
    // Both checks come before anything that names this process as the daemon:
    // a loser that wrote the pidfile or unlinked the socket would strand the
    // winner. The connect probe covers a daemon that predates the lock.
    let Some(mut lock) = try_lock_file(&instance_lock_path_for(socket)?)? else {
        return Ok(None);
    };
    if std::os::unix::net::UnixStream::connect(socket).is_ok() {
        return Ok(None);
    }
    {
        use std::io::Write;
        lock.set_len(0).map_err(DaemonError::PidFile)?;
        write!(lock, "{}", std::process::id()).map_err(DaemonError::PidFile)?;
    }
    write_pid_file(socket)?;

    Ok(Some(InstanceLock {
        socket: socket.to_path_buf(),
        file: std::sync::Mutex::new(lock),
    }))
}

/// Close every descriptor above stderr. A daemon outlives its starter, so a
/// pipe the starter left without close-on-exec (make's jobserver, a CI step's
/// output) would otherwise stay open for the daemon's life and pass on to
/// every pane. Nothing of the daemon's own is open yet.
fn close_inherited_descriptors() {
    use nix::libc;
    #[cfg(target_os = "linux")]
    // SAFETY: close_range takes plain integers; nothing here owns a
    // descriptor above 2 yet.
    if unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) } == 0 {
        return;
    }
    let listed: Option<Vec<i32>> = fs::read_dir("/dev/fd").ok().map(|dir| {
        dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
            .collect()
    });
    // Unlistable: try every number the limit allows.
    // SAFETY: getdtablesize takes no arguments and only reads a limit.
    let fds = listed.unwrap_or_else(|| (3..unsafe { libc::getdtablesize() }).collect());
    // The listing's own descriptor is among them; it closed with the listing.
    for fd in fds.into_iter().filter(|fd| *fd > 2) {
        // SAFETY: nothing in this process owns a descriptor above 2 yet, so
        // closing one cannot pull it out from under a live handle.
        unsafe { libc::close(fd) };
    }
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
        // SAFETY: dup2 only duplicates the already-open /dev/null fd onto the
        // standard file descriptor numbers in the daemon child.
        unsafe {
            let _ = nix::libc::dup2(fd, 0); // stdin
            let _ = nix::libc::dup2(fd, 1); // stdout
            let _ = nix::libc::dup2(fd, 2); // stderr
        }
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

    /// A long resolved directory reached by a short symlink, for the
    /// socket-address-limit cases.
    fn long_dir() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("r".repeat(120));
        std::fs::create_dir_all(real.join("sub")).unwrap();
        (root, real)
    }

    /// The endpoint names the same socket as the spelling, `..` after a
    /// symlink included: the kernel resolves `..` from the link's target.
    #[test]
    fn a_socket_endpoint_names_the_socket_it_was_given() {
        let (root, real) = long_dir();
        let link = root.path().join("l");
        std::os::unix::fs::symlink(real.join("sub"), &link).unwrap();
        let spelling = link.join("..").join("s.sock");
        let endpoint = socket_endpoint(&spelling);
        assert!(
            nix::sys::socket::UnixAddr::new(&endpoint).is_ok(),
            "{}",
            endpoint.display()
        );
        assert_eq!(
            socket_key(&endpoint),
            socket_key(&spelling),
            "{}",
            endpoint.display()
        );
    }
    use std::sync::{Mutex, OnceLock};

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn env_lock() -> &'static Mutex<()> {
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    /// Helper to set XDG_RUNTIME_DIR for testing.
    ///
    /// SAFETY: Callers must hold `ENV_LOCK`. `set_var`/`remove_var` are unsafe
    /// in edition 2024 because env vars are process-global shared mutable state.
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
        let _guard = env_lock().lock().unwrap();
        let original = std::env::var("XDG_RUNTIME_DIR").ok();
        // SAFETY: Guarded by ENV_LOCK and restored before releasing it.
        unsafe { set_xdg_runtime_dir("/tmp/test-shux-xdg") };

        let dir = runtime_dir().unwrap();
        assert_eq!(dir, PathBuf::from("/tmp/test-shux-xdg/shux"));

        unsafe { restore_xdg_runtime_dir(original) };
    }

    // ── issue #105 hardening: the runtime-dir + pidfile writes must not follow
    //    a planted symlink (CWE-59), so the segment-config fix's "private 0700
    //    dir" invariant actually holds. ─────────────────────────────────────────

    #[test]
    fn ensure_runtime_dir_refuses_symlinked_runtime_dir() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = env_lock().lock().unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        // Attacker's target directory (they want it chmod'd / written into).
        let victim = tmp.path().join("victim_dir");
        fs::create_dir(&victim).unwrap();
        fs::set_permissions(&victim, fs::Permissions::from_mode(0o755)).unwrap();
        // XDG_RUNTIME_DIR/shux is pre-planted as a symlink to the victim dir.
        let xdg = tmp.path().join("xdg");
        fs::create_dir(&xdg).unwrap();
        std::os::unix::fs::symlink(&victim, xdg.join("shux")).unwrap();

        let original = std::env::var("XDG_RUNTIME_DIR").ok();
        // SAFETY: guarded by ENV_LOCK, restored before releasing it.
        unsafe { set_xdg_runtime_dir(&xdg) };
        let result = ensure_runtime_dir();
        unsafe { restore_xdg_runtime_dir(original) };

        assert!(
            result.is_err(),
            "ensure_runtime_dir must refuse a symlinked runtime dir, not follow it"
        );
        let mode = fs::metadata(&victim).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o755,
            "victim dir was chmod-followed through the symlink (got {mode:o})"
        );
    }

    #[test]
    fn ensure_runtime_dir_refuses_dir_owned_by_another_uid() {
        use std::os::unix::fs::PermissionsExt;
        // A local user can pre-create the predictable fallback path as a real
        // directory THEY own; O_NOFOLLOW passes it (not a symlink) and fchmod
        // can't change the owner. Creating a foreign-owned dir needs CAP_CHOWN,
        // so this runs only as root (locally / maintainers); it is a no-op on
        // the unprivileged CI runner.
        if !nix::unistd::geteuid().is_root() {
            return;
        }
        let _guard = env_lock().lock().unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let xdg = tmp.path().join("xdg");
        let shuxdir = xdg.join("shux");
        fs::create_dir_all(&shuxdir).unwrap();
        fs::set_permissions(&shuxdir, fs::Permissions::from_mode(0o777)).unwrap();
        let foreign = 12345;
        nix::unistd::chown(
            &shuxdir,
            Some(nix::unistd::Uid::from_raw(foreign)),
            Some(nix::unistd::Gid::from_raw(foreign)),
        )
        .unwrap();

        let original = std::env::var("XDG_RUNTIME_DIR").ok();
        // SAFETY: guarded by ENV_LOCK, restored before releasing it.
        unsafe { set_xdg_runtime_dir(&xdg) };
        let result = ensure_runtime_dir();
        unsafe { restore_xdg_runtime_dir(original) };

        assert!(
            result.is_err(),
            "ensure_runtime_dir must refuse a runtime dir owned by another uid"
        );
        // Untouched: not chmod-tightened to 0700, still owned by the attacker.
        let meta = fs::metadata(&shuxdir).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o777);
    }

    #[test]
    fn write_pid_file_refuses_symlink_and_does_not_clobber() {
        let _guard = env_lock().lock().unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let xdg = tmp.path().join("xdg");
        let shuxdir = xdg.join("shux");
        fs::create_dir_all(&shuxdir).unwrap();
        // Victim file the pidfile path is symlinked onto.
        let victim = tmp.path().join("victim");
        fs::write(&victim, b"KEEP ME").unwrap();
        std::os::unix::fs::symlink(&victim, shuxdir.join("shux.pid")).unwrap();

        let original = std::env::var("XDG_RUNTIME_DIR").ok();
        // SAFETY: guarded by ENV_LOCK, restored before releasing it.
        unsafe { set_xdg_runtime_dir(&xdg) };
        let result = write_pid_file(&socket_path().unwrap());
        unsafe { restore_xdg_runtime_dir(original) };

        assert!(
            result.is_err(),
            "write_pid_file must refuse to write through a symlink"
        );
        assert_eq!(
            fs::read(&victim).unwrap(),
            b"KEEP ME",
            "pidfile write followed the symlink and clobbered the victim"
        );
    }

    #[test]
    fn test_runtime_dir_fallback_without_xdg() {
        let _guard = env_lock().lock().unwrap();
        let original = std::env::var("XDG_RUNTIME_DIR").ok();
        // SAFETY: Guarded by ENV_LOCK and restored before releasing it.
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };

        let dir = runtime_dir().unwrap();
        let uid = nix::unistd::getuid();
        let expected = std::env::temp_dir().join(format!("shux-{uid}"));
        assert_eq!(dir, expected);

        unsafe { restore_xdg_runtime_dir(original) };
    }

    #[test]
    fn test_pid_file_round_trip() {
        let _guard = env_lock().lock().unwrap();
        let tmpdir = tempfile::TempDir::new().unwrap();
        let original = std::env::var("XDG_RUNTIME_DIR").ok();
        // SAFETY: Guarded by ENV_LOCK and restored before releasing it.
        unsafe { set_xdg_runtime_dir(tmpdir.path()) };

        ensure_runtime_dir().unwrap();
        let sock = socket_path().unwrap();
        write_pid_file(&sock).unwrap();

        let pid = read_pid_at(&pid_file_path_for(&sock).unwrap());
        assert_eq!(pid, Some(std::process::id()));

        remove_own_pid_file_for(&sock).unwrap();
        let pid = read_pid_at(&pid_file_path_for(&sock).unwrap());
        assert!(pid.is_none());

        // A pidfile a successor wrote is left in place.
        let path = pid_file_path_for(&sock).unwrap();
        fs::write(&path, "1").unwrap();
        remove_own_pid_file_for(&sock).unwrap();
        assert_eq!(read_pid_at(&path), Some(1));

        unsafe { restore_xdg_runtime_dir(original) };
    }

    /// Two daemons in one runtime dir on different sockets must not share a pidfile.
    ///
    /// They did: the second overwrote the first's entry, and `daemon stop` could
    /// never name the first again -- it ran until the machine went down. The
    /// default socket must keep the documented `$RUNTIME_DIR/shux.pid` path.
    #[test]
    fn a_pidfile_belongs_to_one_socket() {
        let _guard = env_lock().lock().unwrap();
        let tmpdir = tempfile::TempDir::new().unwrap();
        let original = std::env::var("XDG_RUNTIME_DIR").ok();
        // SAFETY: Guarded by ENV_LOCK and restored before releasing it.
        unsafe { set_xdg_runtime_dir(tmpdir.path()) };

        let default_sock = socket_path().unwrap();
        assert_eq!(
            pid_file_path_for(&default_sock)
                .unwrap()
                .file_name()
                .unwrap(),
            "shux.pid",
            "`--socket` documents $XDG_RUNTIME_DIR/shux/shux.pid; keep it true"
        );

        let one = pid_file_path_for(Path::new("/run/x/one.sock")).unwrap();
        let two = pid_file_path_for(Path::new("/run/x/two.sock")).unwrap();
        assert_ne!(one, two, "distinct sockets must get distinct pidfiles");
        assert_ne!(one, pid_file_path_for(&default_sock).unwrap());
        for p in [&one, &two] {
            assert_eq!(
                p.parent().unwrap(),
                runtime_dir().unwrap(),
                "a pidfile must stay inside the ownership-checked 0700 runtime dir"
            );
        }
        // Pinned literally: calling the same pure function twice proves nothing
        // about the cross-version stability this file actually needs.
        assert_eq!(one.file_name().unwrap(), "shux-36a9fb754e91bc3b.pid");

        unsafe { restore_xdg_runtime_dir(original) };
    }

    #[test]
    fn a_held_lock_file_excludes_a_second_holder_until_dropped() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("shux.lock");

        let first = try_lock_file(&path).unwrap().expect("uncontended lock");
        assert!(
            try_lock_file(&path).unwrap().is_none(),
            "a second open of a held lock must be refused"
        );
        drop(first);
        assert!(
            try_lock_file(&path).unwrap().is_some(),
            "dropping the holder must release the lock"
        );
    }

    #[test]
    fn lock_files_are_private_and_refuse_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("shux.lock");
        drop(try_lock_file(&path).unwrap());
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "lock file mode {mode:o}");

        let victim = tmp.path().join("victim");
        fs::write(&victim, b"KEEP ME").unwrap();
        let link = tmp.path().join("linked.lock");
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        assert!(try_lock_file(&link).is_err(), "followed a planted symlink");
        assert_eq!(fs::read(&victim).unwrap(), b"KEEP ME");
    }

    #[test]
    fn a_lock_file_that_is_not_a_regular_file_is_refused() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fifo = tmp.path().join("shux.lock");
        nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRWXU).unwrap();
        let err = try_lock_file(&fifo).expect_err("a FIFO must not pass as a lock file");
        assert!(
            err.to_string().contains(&fifo.display().to_string()),
            "the error must name the file: {err}"
        );
    }

    #[test]
    fn every_spelling_of_a_socket_shares_one_key() {
        let tmp = tempfile::TempDir::new().unwrap();
        let real = tmp.path().join("real");
        fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let want = real.canonicalize().unwrap().join("a.sock");

        for spelling in [
            real.join("a.sock"),
            PathBuf::from(format!("{}//a.sock", real.display())),
            real.join(".").join("a.sock"),
            real.join("x").join("..").join("a.sock"),
            link.join("a.sock"),
        ] {
            assert_eq!(socket_key(&spelling), want, "{}", spelling.display());
        }
        // A directory that does not exist yet keys the same before and after.
        let later = real.join("later").join("a.sock");
        let before = socket_key(&later);
        fs::create_dir(real.join("later")).unwrap();
        assert_eq!(before, socket_key(&later));
        assert_eq!(socket_key(&link.join("later").join("a.sock")), before);
        // Relative to the working directory, as the socket itself is.
        let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
        assert_eq!(socket_key(Path::new("rel.sock")), cwd.join("rel.sock"));
    }

    #[test]
    fn lock_paths_are_keyed_to_the_socket() {
        let _guard = env_lock().lock().unwrap();
        let tmpdir = tempfile::TempDir::new().unwrap();
        let original = std::env::var("XDG_RUNTIME_DIR").ok();
        // SAFETY: Guarded by ENV_LOCK and restored before releasing it.
        unsafe { set_xdg_runtime_dir(tmpdir.path()) };

        let default_sock = socket_path().unwrap();
        let other = Path::new("/run/x/one.sock");
        let dir = runtime_dir().unwrap();
        assert_eq!(
            instance_lock_path_for(&default_sock).unwrap(),
            dir.join("shux.lock")
        );
        assert_eq!(
            start_lock_path_for(&default_sock).unwrap(),
            dir.join("shux.start.lock")
        );
        assert_eq!(
            instance_lock_path_for(other).unwrap(),
            dir.join("shux-36a9fb754e91bc3b.lock")
        );
        assert_eq!(
            attach_socket_path_for(&default_sock).unwrap(),
            dir.join("attach.sock")
        );
        assert_eq!(
            attach_socket_path_for(other).unwrap(),
            dir.join("attach-36a9fb754e91bc3b.sock")
        );

        unsafe { restore_xdg_runtime_dir(original) };
    }

    #[test]
    fn test_remove_nonexistent_pid_file_is_ok() {
        let _guard = env_lock().lock().unwrap();
        let tmpdir = tempfile::TempDir::new().unwrap();
        let original = std::env::var("XDG_RUNTIME_DIR").ok();
        // SAFETY: Guarded by ENV_LOCK and restored before releasing it.
        unsafe { set_xdg_runtime_dir(tmpdir.path()) };

        ensure_runtime_dir().unwrap();
        remove_own_pid_file_for(&socket_path().unwrap()).unwrap();

        unsafe { restore_xdg_runtime_dir(original) };
    }

    /// Startup binds over a missing socket file, replaces a stale one, and
    /// never replaces one that is still served.
    #[tokio::test]
    async fn test_remove_nonexistent_socket_file_is_ok() {
        let tmpdir = tempfile::TempDir::new().unwrap();
        let path = tmpdir.path().join("shux.sock");
        let served = shux_rpc::uds::OwnedSocket::bind(&path).unwrap();
        assert!(std::os::unix::net::UnixStream::connect(&path).is_ok());
        let again = shux_rpc::uds::OwnedSocket::bind(&path);
        assert_eq!(
            again.err().map(|e| e.kind()),
            Some(io::ErrorKind::AddrInUse)
        );
        drop(served);
        assert!(!path.exists(), "dropping the listener removes its own file");
        std::os::unix::net::UnixListener::bind(&path).unwrap();
        // A stale file nobody serves is replaced.
        let _rebound = shux_rpc::uds::OwnedSocket::bind(&path).unwrap();
        assert!(std::os::unix::net::UnixStream::connect(&path).is_ok());
    }
}
