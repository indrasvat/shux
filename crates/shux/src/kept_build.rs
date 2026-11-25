//! Each daemon keeps a file of its own build.
//!
//! An upgrade replaces or deletes the installed binary while daemons of the
//! old build keep running: the running image survives, its path does not. A
//! daemon therefore opens its binary as soon as it has daemonized and, once it
//! serves, keeps those very bytes at `<runtime dir>/builds/<build>/shux`: a
//! clone on macOS, a link to the same inode on Linux, a copy across file
//! systems. Its own helpers (`__share_supervise`) run from it, so they are
//! always its build, and a CLI that cannot speak to it (another wire
//! generation) can hand a command to exactly that build.
//!
//! Never a link on macOS: a new link to a binary can make the system assess
//! it again on its next exec, a cost the next `shux` command would pay.
//!
//! Each daemon holds a shared lock on its build's `lock` file while it runs;
//! a build no daemon holds is removed by the next daemon to keep one.

use std::fs;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

/// Set on a command handed to the daemon's own build: `<that build's id>
/// <pid> <caller version> <caller sha>`. Only the process `<pid>` honours it
/// (exec keeps the pid, so an exported or inherited value is ignored); it
/// then trusts that daemon as its own, speaks for the caller, and never hands
/// off again.
pub(crate) const HANDOFF_ENV: &str = "SHUX_HANDOFF";

/// A command handed to this build by [`hand_off`].
pub struct Handoff {
    /// The build it was handed to: the daemon's.
    pub id: String,
    pub caller_version: String,
    pub caller_git_sha: String,
}

fn parse_handoff(raw: &str, pid: u32) -> Option<Handoff> {
    let mut f = raw.split(' ');
    let (id, for_pid, version, sha) = (f.next()?, f.next()?, f.next()?, f.next()?);
    let valid = f.next().is_none()
        && for_pid.parse() == Ok(pid)
        && !id.is_empty()
        && crate::client::semver(version).is_some()
        && !sha.is_empty();
    valid.then(|| Handoff {
        id: id.to_string(),
        caller_version: version.to_string(),
        caller_git_sha: sha.to_string(),
    })
}

/// This command's hand-off, when it is one: see [`HANDOFF_ENV`].
pub fn handoff() -> Option<&'static Handoff> {
    static HANDOFF: OnceLock<Option<Handoff>> = OnceLock::new();
    HANDOFF
        .get_or_init(|| parse_handoff(&std::env::var(HANDOFF_ENV).ok()?, std::process::id()))
        .as_ref()
}

/// A build no daemon holds is kept this long after it was made or last
/// touched: a daemon between making its copy and locking it is not robbed.
const UNUSED_GRACE: Duration = Duration::from_secs(10 * 60);

/// How often a daemon refreshes its copy's times, ahead of a tmp cleaner
/// that removes files nobody touched for days.
pub(crate) const TOUCH_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// The binary this daemon runs, opened right after it daemonized.
static IMAGE: OnceLock<Result<fs::File, String>> = OnceLock::new();

/// This daemon's kept build.
struct Kept {
    exe: PathBuf,
    dev: u64,
    ino: u64,
    /// Shared lock: in use while this daemon lives.
    _lock: fs::File,
}

static KEPT: OnceLock<Result<Kept, String>> = OnceLock::new();

/// This build's id: `<version>-<git sha>`.
pub(crate) fn build_id() -> String {
    format!("{}-{}", shux_rpc::VERSION, shux_rpc::GIT_SHA)
}

/// `<runtime dir>/builds`, made if missing: a real directory of this user
/// that only this user can write, never followed through a link.
fn builds_dir() -> Result<PathBuf, String> {
    let dir = crate::daemon::ensure_runtime_dir()
        .map_err(|e| format!("runtime dir: {e}"))?
        .join("builds");
    match fs::create_dir(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    }
    private_dir(&dir)?;
    Ok(dir)
}

/// `dir` is a directory, not a link, of this user, writable by nobody else.
fn private_dir(dir: &Path) -> Result<(), String> {
    let meta = fs::symlink_metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !meta.is_dir() || meta.uid() != nix::unistd::getuid().as_raw() {
        return Err(format!(
            "{} is not a directory of this user (a link, a file, or another owner's)",
            dir.display()
        ));
    }
    if meta.mode() & 0o077 != 0 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    Ok(())
}

/// Right after daemonizing: hold the binary this daemon runs, before an
/// install can replace the path. On Linux the running image itself.
pub(crate) fn open_image() {
    let exe = if cfg!(target_os = "linux") {
        Ok(PathBuf::from("/proc/self/exe"))
    } else {
        std::env::current_exe()
    };
    let image = exe
        .and_then(fs::File::open)
        .map_err(|e| format!("open this daemon's binary: {e}"));
    let _ = IMAGE.set(image);
}

/// Once the daemon serves, off its request path: keep the binary it holds,
/// lock it, remove the builds no daemon holds, and write the build file.
pub(crate) fn keep(socket: &Path) {
    let kept = (|| {
        let image = IMAGE
            .get()
            .ok_or("the binary was never opened")?
            .as_ref()
            .map_err(Clone::clone)?;
        let meta = image.metadata().map_err(|e| format!("binary: {e}"))?;
        let builds = builds_dir()?;
        let dir = builds.join(format!(
            "{}-{}-{}-{}",
            build_id(),
            meta.dev(),
            meta.ino(),
            meta.len()
        ));
        let exe = dir.join("shux");
        // Locked before it is judged, so a sweep cannot remove it after.
        for _ in 0..3 {
            if fs::symlink_metadata(&dir).is_err() {
                publish(image, &builds, &dir)?;
            }
            let lock = held_lock(&dir)?;
            if private_dir(&dir).is_ok() && same_bytes(image, &exe) {
                let kept =
                    fs::symlink_metadata(&exe).map_err(|e| format!("{}: {e}", exe.display()))?;
                return Ok(Kept {
                    dev: kept.dev(),
                    ino: kept.ino(),
                    exe,
                    _lock: lock,
                });
            }
            // Not this binary's bytes (a plant, a crash mid-copy): replaced.
            drop(lock);
            let _ = fs::remove_dir_all(&dir);
        }
        Err(format!("could not keep the build at {}", dir.display()))
    })();
    if let Err(e) = &kept {
        tracing::warn!(error = %e, "could not keep this daemon's build");
    }
    let _ = KEPT.set(kept);
    if let Ok(builds) = builds_dir() {
        remove_unused(&builds, SystemTime::now());
    }
    write_build_file(socket);
}

/// A shared lock on `<dir>/lock` that is still the dir's own: a sweep that
/// took the dir away before the lock landed is seen, not trusted.
fn held_lock(dir: &Path) -> Result<fs::File, String> {
    let path = dir.join("lock");
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    lock.lock_shared().map_err(|e| format!("lock: {e}"))?;
    let held = lock.metadata().map_err(|e| format!("lock: {e}"))?;
    let named = fs::symlink_metadata(&path).map_err(|e| format!("lock: {e}"))?;
    if (held.dev(), held.ino()) != (named.dev(), named.ino()) {
        return Err(format!("{} was swept as it was locked", dir.display()));
    }
    Ok(lock)
}

/// Whether `exe` is a regular file of this user holding exactly `image`'s
/// bytes: the same inode, or the same contents.
fn same_bytes(image: &fs::File, exe: &Path) -> bool {
    let (Ok(want), Ok(have)) = (image.metadata(), fs::symlink_metadata(exe)) else {
        return false;
    };
    if !have.is_file() || have.uid() != nix::unistd::getuid().as_raw() || have.mode() & 0o022 != 0 {
        return false;
    }
    if (want.dev(), want.ino()) == (have.dev(), have.ino()) {
        return true;
    }
    if want.len() != have.len() {
        return false;
    }
    same_contents(image, exe, want.len()).unwrap_or(false)
}

/// Whether the `len` bytes of `image` and of `exe` are equal.
fn same_contents(image: &fs::File, exe: &Path, len: u64) -> std::io::Result<bool> {
    use std::io::{Read as _, Seek as _};
    let mut a = image.try_clone()?;
    a.rewind()?;
    let mut b = fs::File::open(exe)?;
    let (mut x, mut y) = (vec![0u8; 1 << 16], vec![0u8; 1 << 16]);
    let mut left = len;
    while left > 0 {
        let n = usize::try_from(left.min(x.len() as u64)).unwrap_or(x.len());
        a.read_exact(&mut x[..n])?;
        b.read_exact(&mut y[..n])?;
        if x[..n] != y[..n] {
            return Ok(false);
        }
        left -= n as u64;
    }
    Ok(true)
}

/// Make `<dir>/shux` from `image`, published whole by one rename.
fn publish(image: &fs::File, builds: &Path, dir: &Path) -> Result<(), String> {
    let staging = builds.join(format!(".new-{}", std::process::id()));
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    let placed = materialize(image, &staging.join("shux")).and_then(|()| fs::rename(&staging, dir));
    match placed {
        Ok(()) => Ok(()),
        // Another daemon of this very binary published it first.
        Err(_) if dir.join("shux").is_file() => {
            let _ = fs::remove_dir_all(&staging);
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_dir_all(&staging);
            Err(format!("keep the build at {}: {e}", dir.display()))
        }
    }
}

/// The bytes of `image` at `to`: a clone or a link where the system makes one
/// from the open file, a copy otherwise.
fn materialize(image: &fs::File, to: &Path) -> std::io::Result<()> {
    use std::os::fd::AsRawFd as _;
    let fd = image.as_raw_fd();
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let dst = std::ffi::CString::new(to.as_os_str().as_bytes())?;
        // SAFETY: `fd` is open for the call's length, `dst` is a NUL-ended
        // path that outlives it; the call only reads both.
        if unsafe { nix::libc::fclonefileat(fd, nix::libc::AT_FDCWD, dst.as_ptr(), 0) } == 0 {
            // A clone is its own file, with the install's mode: made private.
            return fs::set_permissions(to, fs::Permissions::from_mode(0o755));
        }
    }
    // A link shares the install's mode, so only one nobody else can write.
    #[cfg(target_os = "linux")]
    if image.metadata()?.mode() & 0o022 == 0 {
        let proc_fd = format!("/proc/self/fd/{fd}");
        let linked = nix::unistd::linkat(
            nix::fcntl::AT_FDCWD,
            proc_fd.as_str(),
            nix::fcntl::AT_FDCWD,
            to,
            nix::fcntl::AtFlags::AT_SYMLINK_FOLLOW,
        );
        if linked.is_ok() {
            return Ok(());
        }
    }
    let mut src = image.try_clone()?;
    std::io::Seek::rewind(&mut src)?;
    let mut dst = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)?;
    std::io::copy(&mut src, &mut dst)?;
    dst.set_permissions(fs::Permissions::from_mode(0o755))?;
    dst.sync_all()
}

/// The binary this daemon runs a helper of its own with: its kept build, so
/// the helper is this build however the installed binary changed since.
pub(crate) fn helper_exe() -> std::io::Result<PathBuf> {
    match KEPT.get() {
        Some(Ok(k)) => Ok(k.exe.clone()),
        _ => std::env::current_exe(),
    }
}

/// What `system.version` and the daemon's build file say of its copy:
/// `{exe, dev, ino}`, or `{error}` when it could not keep one.
pub(crate) fn advertised() -> serde_json::Value {
    match KEPT.get() {
        Some(Ok(k)) => serde_json::json!({ "exe": k.exe, "dev": k.dev, "ino": k.ino }),
        Some(Err(e)) => serde_json::json!({ "exe": null, "error": e }),
        None => serde_json::json!({ "exe": null, "error": "not kept yet" }),
    }
}

/// Refresh the kept build's times, so an age-based tmp cleaner leaves it
/// alone. Access time only on the binary: on Linux it may be the installed
/// file itself.
pub(crate) fn touch() {
    if let Some(Ok(k)) = KEPT.get() {
        let now = SystemTime::now();
        let dir = k.exe.parent().unwrap_or(&k.exe);
        let everything = fs::FileTimes::new().set_accessed(now).set_modified(now);
        let touched = fs::File::open(&k.exe)
            .and_then(|f| f.set_times(fs::FileTimes::new().set_accessed(now)))
            .and_then(|()| fs::File::open(dir)?.set_times(everything))
            .and_then(|()| fs::File::open(dir.join("lock"))?.set_times(everything));
        if let Err(e) = touched {
            tracing::warn!(build = %dir.display(), error = %e, "could not refresh kept build");
        }
    }
}

/// Remove every build in `builds` that no daemon holds and nobody touched
/// within [`UNUSED_GRACE`] of `now`, leftovers of a failed copy included.
pub(crate) fn remove_unused(builds: &Path, now: SystemTime) {
    let Ok(entries) = fs::read_dir(builds) else {
        return;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        // Never followed: only a real directory of this user is swept.
        let Ok(meta) = fs::symlink_metadata(&dir) else {
            continue;
        };
        if !meta.is_dir() || meta.uid() != nix::unistd::getuid().as_raw() {
            continue;
        }
        let fresh = meta
            .modified()
            .is_ok_and(|t| now.duration_since(t).unwrap_or_default() < UNUSED_GRACE);
        if fresh {
            continue;
        }
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(dir.join("lock"));
        let unused = match &lock {
            Ok(file) => file.try_lock().is_ok(),
            // A staging dir that cannot hold a lock file: a crash's leftover.
            Err(_) => true,
        };
        if !unused {
            continue;
        }
        // Moved aside while still locked, so a daemon that locks it next sees
        // its lock is no longer the dir's (`held_lock`) and makes its own.
        let tomb = builds.join(format!(".dead-{}-{}", std::process::id(), meta.ino()));
        let removed = fs::rename(&dir, &tomb).and_then(|()| fs::remove_dir_all(&tomb));
        drop(lock);
        match removed {
            Ok(()) => tracing::info!(build = %dir.display(), "removed an unused kept build"),
            Err(e) => tracing::warn!(build = %dir.display(), error = %e, "could not remove"),
        }
    }
}

/// The daemon's build file beside its pidfile: what a CLI of another wire
/// generation reads to hand a command to the daemon's own build, without
/// speaking a protocol it may not share.
pub(crate) fn build_file_for(socket: &Path) -> Result<PathBuf, crate::daemon::DaemonError> {
    Ok(crate::daemon::pid_file_path_for(socket)?.with_extension("build.json"))
}

/// The socket this daemon serves, once it keeps its build.
static SOCKET: OnceLock<PathBuf> = OnceLock::new();

/// Set once the daemon retires: its build file then outlives it, saying so.
static RETIRING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The daemon retires: say so in its build file, which it then leaves behind,
/// so a client whose call it dropped knows nothing of that call survives.
pub(crate) fn mark_retiring() {
    shux_rpc::server::mark_retiring();
    RETIRING.store(true, std::sync::atomic::Ordering::SeqCst);
    if let Some(socket) = SOCKET.get() {
        write_build_file(socket);
    }
}

/// Whether daemon `pid`, now gone, retired from `socket`, as its build file
/// says. A daemon that was stopped or crashed did not.
pub(crate) fn retired(socket: &Path, pid: u32) -> bool {
    read_build_file(socket).is_some_and(|f| f["pid"] == pid && f["retiring"] == true)
}

/// The build file for `socket`: a regular file, never through a link, read
/// only up to a size no build file reaches.
fn read_build_file(socket: &Path) -> Option<serde_json::Value> {
    use std::io::Read as _;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(build_file_for(socket).ok()?)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(64 * 1024).read_to_string(&mut text).ok()?;
    serde_json::from_str(&text).ok()
}

/// Write this daemon's build file.
fn write_build_file(socket: &Path) {
    // Startup and retirement both write it: one at a time, or one could
    // publish the other's half-written file.
    static WRITING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let _ = SOCKET.set(socket.to_path_buf());
    let written = (|| {
        let path = build_file_for(socket).map_err(|e| e.to_string())?;
        let mut answer = shux_rpc::server::version_answer();
        answer["pid"] = std::process::id().into();
        answer["retiring"] = RETIRING.load(std::sync::atomic::Ordering::SeqCst).into();
        answer["build"] = advertised();
        let tmp = path.with_extension(format!("build.json.{}.tmp", std::process::id()));
        fs::write(&tmp, answer.to_string()).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &path).map_err(|e| e.to_string())
    })();
    if let Err(e) = written {
        tracing::warn!(error = %e, "could not write the daemon's build file");
    }
}

/// The build a command was handed to, when it was: see [`HANDOFF_ENV`].
pub fn handed_to() -> Option<String> {
    handoff().map(|h| h.id.clone())
}

/// Before a command that talks to the daemon: when the daemon serving
/// `socket` speaks another wire generation, run this very command (argv,
/// environment and stdin untouched) with that daemon's own build, and do not
/// return. Returns when the daemon speaks this generation, none serves the
/// socket, or no usable copy of its build is there: the command then goes on,
/// and the daemon is refused or served as its version answer decides.
pub(crate) fn hand_off(socket: &Path) {
    if handed_to().is_some() {
        return;
    }
    let Some((exe, id)) = handoff_target(socket) else {
        return;
    };
    {
        use std::io::IsTerminal as _;
        if std::io::stderr().is_terminal() {
            crate::style::print_muted_note(&format!(
                "running this with shux {id}, the running daemon's build \
                 (it speaks another protocol than this shux {})",
                build_id()
            ));
        }
    }
    use std::os::unix::process::CommandExt as _;
    let err = std::process::Command::new(&exe)
        .args(std::env::args_os().skip(1))
        .env(
            HANDOFF_ENV,
            format!(
                "{id} {} {} {}",
                std::process::id(),
                shux_rpc::VERSION,
                shux_rpc::GIT_SHA
            ),
        )
        .exec();
    tracing::warn!(exe = %exe.display(), error = %err, "could not hand the command off");
}

/// The kept build of the daemon serving `socket` and its id, when that daemon
/// is live, speaks another wire generation, and its copy is the very file it
/// recorded: a regular file of this user, writable by nobody else.
fn handoff_target(socket: &Path) -> Option<(PathBuf, String)> {
    let file = read_build_file(socket)?;
    let wire = file.get("wire")?.as_u64()?;
    if wire == u64::from(shux_rpc::WIRE_GENERATION) {
        return None;
    }
    let pid = u32::try_from(file.get("pid")?.as_u64()?).ok()?;
    if !crate::daemon_boot::is_live_shux_daemon(pid, socket) {
        return None;
    }
    let id = format!(
        "{}-{}",
        file.get("version")?.as_str()?,
        file.get("git_sha")?.as_str()?
    );
    let build = file.get("build")?;
    let exe = PathBuf::from(build.get("exe")?.as_str()?);
    // In this runtime dir's own `builds`, through directories only this
    // user can write.
    let key_dir = exe.parent()?;
    let builds = key_dir.parent()?;
    let expected = crate::daemon::runtime_dir().ok()?.join("builds");
    let ours = |dir: &Path| {
        fs::symlink_metadata(dir).is_ok_and(|m| {
            m.is_dir() && m.uid() == nix::unistd::getuid().as_raw() && m.mode() & 0o022 == 0
        })
    };
    if builds != expected || !ours(builds) || !ours(key_dir) {
        tracing::warn!(exe = %exe.display(), "the daemon's kept build is not in a private builds dir");
        return None;
    }
    let meta = fs::symlink_metadata(&exe).ok()?;
    let ours = meta.is_file()
        && meta.uid() == nix::unistd::getuid().as_raw()
        && meta.mode() & 0o022 == 0
        && Some(meta.dev()) == build.get("dev")?.as_u64()
        && Some(meta.ino()) == build.get("ino")?.as_u64();
    if !ours {
        tracing::warn!(exe = %exe.display(), "the daemon's kept build is not the file it recorded");
        return None;
    }
    Some((exe, id))
}

/// Remove this daemon's build file as it leaves, unless it retired.
pub(crate) fn remove_build_file(socket: &Path) {
    if RETIRING.load(std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    if let Ok(path) = build_file_for(socket) {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The moment the sweep runs, as data: no clock is read.
    const NOW: Duration = Duration::from_secs(2_000_000_000);

    fn build_dir(root: &Path, name: &str, age: Duration) -> PathBuf {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("shux"), b"bin").unwrap();
        let then = SystemTime::UNIX_EPOCH + NOW - age;
        fs::File::open(&dir)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(then))
            .unwrap();
        dir
    }

    /// A build a live daemon holds survives however old it is; one nobody
    /// holds goes once past the grace, and not before.
    #[test]
    fn only_old_builds_no_daemon_holds_are_removed() {
        let root = tempfile::tempdir().unwrap();
        let now = SystemTime::UNIX_EPOCH + NOW;
        let old = UNUSED_GRACE * 2;
        let held = build_dir(root.path(), "held", old);
        let unused = build_dir(root.path(), "unused", old);
        let fresh = build_dir(root.path(), "fresh", UNUSED_GRACE / 2);
        let lock = fs::File::create(held.join("lock")).unwrap();
        lock.lock_shared().unwrap();

        remove_unused(root.path(), now);
        assert!(held.exists(), "a held build was removed");
        assert!(fresh.exists(), "a build inside its grace was removed");
        assert!(!unused.exists(), "an unused build was kept");

        drop(lock);
        remove_unused(root.path(), now);
        assert!(!held.exists(), "a released build outlived its daemon");
    }

    /// Writes of the build file from several threads (startup's and
    /// retirement's) never publish a half-written file.
    #[test]
    fn concurrent_build_file_writes_publish_whole_files() {
        let runtime = tempfile::tempdir().unwrap();
        // SAFETY: nextest runs this test alone in its process.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", runtime.path()) };
        let dir = crate::daemon::ensure_runtime_dir().unwrap();
        let socket = dir.join("t.sock");
        let path = build_file_for(&socket).unwrap();
        let writers: Vec<_> = (0..8)
            .map(|_| {
                let socket = socket.clone();
                std::thread::spawn(move || {
                    for _ in 0..200 {
                        write_build_file(&socket);
                    }
                })
            })
            .collect();
        let mut seen = 0;
        while writers.iter().any(|w| !w.is_finished()) {
            if let Ok(text) = fs::read_to_string(&path) {
                let parsed: Result<serde_json::Value, _> = serde_json::from_str(&text);
                assert!(parsed.is_ok(), "a half-written build file: {text:?}");
                seen += 1;
            }
        }
        for w in writers {
            w.join().unwrap();
        }
        assert!(seen > 0, "never read a build file");
    }

    /// The kept build is the bytes of the open binary even after its path
    /// is replaced: what a daemon keeps is what it runs.
    #[test]
    fn the_kept_build_is_the_open_binary_not_the_path() {
        let root = tempfile::tempdir().unwrap();
        let installed = root.path().join("shux");
        fs::write(&installed, b"old build").unwrap();
        let image = fs::File::open(&installed).unwrap();
        // An install replaces the file, as `install` and package managers do.
        fs::remove_file(&installed).unwrap();
        fs::write(&installed, b"new build").unwrap();

        let kept = root.path().join("kept");
        materialize(&image, &kept).unwrap();
        assert_eq!(fs::read(&kept).unwrap(), b"old build");
    }
}
