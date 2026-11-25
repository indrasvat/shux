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
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

/// Set on a command handed to the daemon's own build: that build's id. The
/// command trusts that daemon as its own and never hands off again.
pub(crate) const HANDOFF_ENV: &str = "SHUX_HANDOFF";

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

fn builds_dir() -> Result<PathBuf, String> {
    let dir = crate::daemon::ensure_runtime_dir()
        .map_err(|e| format!("runtime dir: {e}"))?
        .join("builds");
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir)
}

/// Right after daemonizing: hold the binary this daemon runs, before an
/// install can replace the path.
pub(crate) fn open_image() {
    let image = std::env::current_exe()
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
        if !fs::metadata(&exe).is_ok_and(|m| m.is_file() && m.len() == meta.len()) {
            publish(image, &builds, &dir)?;
        }
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("lock"))
            .map_err(|e| format!("lock: {e}"))?;
        lock.lock_shared().map_err(|e| format!("lock: {e}"))?;
        let kept = fs::metadata(&exe).map_err(|e| format!("{}: {e}", exe.display()))?;
        Ok::<_, String>(Kept {
            dev: kept.dev(),
            ino: kept.ino(),
            exe,
            _lock: lock,
        })
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
            return Ok(());
        }
    }
    #[cfg(target_os = "linux")]
    {
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
        let fresh = fs::metadata(&dir)
            .and_then(|m| m.modified())
            .is_ok_and(|t| now.duration_since(t).unwrap_or_default() < UNUSED_GRACE);
        if fresh {
            continue;
        }
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("lock"));
        let unused = match &lock {
            Ok(file) => file.try_lock().is_ok(),
            // A staging dir with no lock file yet, or not a dir at all.
            Err(_) => true,
        };
        if unused {
            match fs::remove_dir_all(&dir) {
                Ok(()) => tracing::info!(build = %dir.display(), "removed an unused kept build"),
                Err(e) => tracing::warn!(build = %dir.display(), error = %e, "could not remove"),
            }
        }
    }
}

/// The daemon's build file beside its pidfile: what a CLI of another wire
/// generation reads to hand a command to the daemon's own build, without
/// speaking a protocol it may not share.
pub(crate) fn build_file_for(socket: &Path) -> Result<PathBuf, crate::daemon::DaemonError> {
    Ok(crate::daemon::pid_file_path_for(socket)?.with_extension("build.json"))
}

/// Write this daemon's build file.
fn write_build_file(socket: &Path) {
    let written = (|| {
        let path = build_file_for(socket).map_err(|e| e.to_string())?;
        let mut answer = shux_rpc::server::version_answer();
        answer["pid"] = std::process::id().into();
        answer["build"] = advertised();
        let tmp = path.with_extension("build.json.tmp");
        fs::write(&tmp, answer.to_string()).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &path).map_err(|e| e.to_string())
    })();
    if let Err(e) = written {
        tracing::warn!(error = %e, "could not write the daemon's build file");
    }
}

/// The build a command was handed to, when it was: see [`HANDOFF_ENV`].
pub fn handed_to() -> Option<String> {
    std::env::var(HANDOFF_ENV).ok().filter(|id| !id.is_empty())
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
        .env(HANDOFF_ENV, &id)
        .exec();
    tracing::warn!(exe = %exe.display(), error = %err, "could not hand the command off");
}

/// The kept build of the daemon serving `socket` and its id, when that daemon
/// is live, speaks another wire generation, and its copy is the very file it
/// recorded: a regular file of this user, writable by nobody else.
fn handoff_target(socket: &Path) -> Option<(PathBuf, String)> {
    let text = fs::read_to_string(build_file_for(socket).ok()?).ok()?;
    let file: serde_json::Value = serde_json::from_str(&text).ok()?;
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

/// Remove this daemon's build file as it leaves.
pub(crate) fn remove_build_file(socket: &Path) {
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
