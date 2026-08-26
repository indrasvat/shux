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
    let dir = runtime_dir()?;
    if socket_path().is_ok_and(|default| default == socket) {
        return Ok(dir.join("shux.pid"));
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in socket.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    Ok(dir.join(format!("shux-{hash:016x}.pid")))
}

/// Pidfile paths to consult for `socket`, most specific first.
///
/// The second is the pre-upgrade location: every shux before this change wrote
/// `$RUNTIME_DIR/shux.pid` whatever socket it served, so skipping it would orphan
/// the daemon being replaced at every upgrade. Safe because the caller
/// identity-checks the pid it finds against the socket that process serves.
pub fn pid_file_candidates(socket: &Path) -> Result<Vec<PathBuf>, DaemonError> {
    let primary = pid_file_path_for(socket)?;
    let legacy = runtime_dir()?.join("shux.pid");
    Ok(if primary == legacy {
        vec![primary]
    } else {
        vec![primary, legacy]
    })
}

/// Read a pid from an explicit pidfile path.
pub fn read_pid_at(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Full path to the Unix domain socket: `$RUNTIME_DIR/shux.sock`
pub fn socket_path() -> Result<PathBuf, DaemonError> {
    Ok(runtime_dir()?.join("shux.sock"))
}

/// Full path to the attach-session socket: `$RUNTIME_DIR/attach.sock`.
/// Distinct from the JSON-RPC socket so the attach loop doesn't have to
/// multiplex protocols on a single connection.
pub fn attach_socket_path() -> Result<PathBuf, DaemonError> {
    Ok(runtime_dir()?.join("attach.sock"))
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

/// Remove the PID file (called on shutdown).
pub fn remove_pid_file_for(socket: &Path) -> Result<(), DaemonError> {
    let path = pid_file_path_for(socket)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(DaemonError::RemovePidFile(e)),
    }
}

/// Remove the socket file (called before binding and on shutdown).
pub fn remove_socket_file_for(socket: &Path) -> Result<(), DaemonError> {
    match fs::remove_file(socket) {
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
pub fn daemonize(socket: &Path) -> Result<bool, DaemonError> {
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
    write_pid_file(socket)?;

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

        remove_pid_file_for(&sock).unwrap();
        let pid = read_pid_at(&pid_file_path_for(&sock).unwrap());
        assert!(pid.is_none());

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
    fn test_remove_nonexistent_pid_file_is_ok() {
        let _guard = env_lock().lock().unwrap();
        let tmpdir = tempfile::TempDir::new().unwrap();
        let original = std::env::var("XDG_RUNTIME_DIR").ok();
        // SAFETY: Guarded by ENV_LOCK and restored before releasing it.
        unsafe { set_xdg_runtime_dir(tmpdir.path()) };

        ensure_runtime_dir().unwrap();
        remove_pid_file_for(&socket_path().unwrap()).unwrap();

        unsafe { restore_xdg_runtime_dir(original) };
    }

    #[test]
    fn test_remove_nonexistent_socket_file_is_ok() {
        let tmpdir = tempfile::TempDir::new().unwrap();
        let original = std::env::var("XDG_RUNTIME_DIR").ok();
        // SAFETY: See above
        unsafe { set_xdg_runtime_dir(tmpdir.path()) };

        ensure_runtime_dir().unwrap();
        remove_socket_file_for(&socket_path().unwrap()).unwrap();

        unsafe { restore_xdg_runtime_dir(original) };
    }
}
