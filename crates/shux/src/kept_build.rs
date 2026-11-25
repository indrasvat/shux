//! A daemon runs from its own copy of the binary that started it.
//!
//! An upgrade replaces or deletes the installed binary while daemons of the
//! old build keep running. Each daemon therefore execs itself, before it
//! daemonizes, from `<runtime dir>/builds/<build>/shux`: a hard link where the
//! file system allows (Linux refuses to rewrite a running binary in place) and
//! a copy otherwise (APFS clones it). Its own helpers (`__share_supervise`)
//! then always run its own build, and a CLI that cannot speak to it (another
//! wire generation) can hand a command to exactly that build.
//!
//! Each daemon holds a shared lock on its build's `lock` file while it runs;
//! a build no daemon holds is removed by the next daemon to start.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

/// Set on the exec into the kept copy: the path it was exec'd as.
const KEPT_ENV: &str = "SHUX_KEPT_EXE";

/// Set on a command handed to the daemon's own build: that build's id. The
/// command trusts that daemon as its own and never hands off again.
pub(crate) const HANDOFF_ENV: &str = "SHUX_HANDOFF";

/// A build no daemon holds is kept this long after it was made or last
/// touched: a daemon between making its copy and locking it is not robbed.
const UNUSED_GRACE: Duration = Duration::from_secs(10 * 60);

/// How often a daemon refreshes its copy's times, ahead of a tmp cleaner
/// that removes files nobody touched for days.
pub(crate) const TOUCH_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// The copy this daemon runs from, once adopted.
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

/// In `__daemon`, first thing: exec this build from its kept copy, making it
/// if needed. Returns when already running from it, or when no copy could be
/// made or run, which is logged and advertised; the daemon then runs from
/// the installed binary.
pub(crate) fn exec_from_kept_copy() {
    if let Some(kept) = std::env::var_os(KEPT_ENV) {
        // SAFETY: called first thing in `__daemon`, before any thread exists.
        unsafe { std::env::remove_var(KEPT_ENV) };
        KEPT_PATH.get_or_init(|| PathBuf::from(kept));
        return;
    }
    let target = match make_copy() {
        Ok(target) => target,
        Err(e) => {
            let _ = KEPT.set(Err(e));
            return;
        }
    };
    use std::os::unix::process::CommandExt as _;
    let err = std::process::Command::new(&target)
        .args(std::env::args_os().skip(1))
        .env(KEPT_ENV, &target)
        .exec();
    let _ = KEPT.set(Err(format!("exec {}: {err}", target.display())));
}

/// The path `exec_from_kept_copy` found itself exec'd as.
static KEPT_PATH: OnceLock<PathBuf> = OnceLock::new();

/// After daemonizing: lock the copy this daemon runs from, so no other daemon
/// removes it, and remove the builds no daemon holds.
pub(crate) fn adopt() {
    let Some(exe) = KEPT_PATH.get().cloned() else {
        return;
    };
    let adopted = (|| {
        let dir = exe.parent().ok_or("kept copy has no directory")?;
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("lock"))
            .map_err(|e| format!("lock: {e}"))?;
        lock.lock_shared().map_err(|e| format!("lock: {e}"))?;
        let meta = fs::metadata(&exe).map_err(|e| format!("{}: {e}", exe.display()))?;
        Ok::<_, String>(Kept {
            dev: meta.dev(),
            ino: meta.ino(),
            exe: exe.clone(),
            _lock: lock,
        })
    })();
    if let Err(e) = &adopted {
        tracing::warn!(error = %e, "could not hold this daemon's kept build");
    }
    let _ = KEPT.set(adopted);
    if let Ok(dir) = builds_dir() {
        remove_unused(&dir, SystemTime::now());
    }
}

/// What `system.version` and the daemon's build file say of its copy:
/// `{exe, dev, ino}`, or `{error}` when it runs from the installed binary.
pub(crate) fn advertised() -> serde_json::Value {
    match KEPT.get() {
        Some(Ok(k)) => serde_json::json!({ "exe": k.exe, "dev": k.dev, "ino": k.ino }),
        Some(Err(e)) => serde_json::json!({ "exe": null, "error": e }),
        None => serde_json::json!({ "exe": null, "error": "not started from a kept copy" }),
    }
}

/// Refresh the copy's times, so an age-based tmp cleaner leaves it alone.
pub(crate) fn touch() {
    if let Some(Ok(k)) = KEPT.get() {
        for path in [k.exe.as_path(), k.exe.parent().unwrap_or(&k.exe)] {
            if let Err(e) = touch_path(path) {
                tracing::warn!(path = %path.display(), error = %e, "could not refresh kept build");
            }
        }
    }
}

fn touch_path(path: &Path) -> std::io::Result<()> {
    let now = fs::FileTimes::new()
        .set_accessed(SystemTime::now())
        .set_modified(SystemTime::now());
    fs::File::open(path)?.set_times(now)
}

/// `<builds>/<id>-<size>-<mtime>/shux` for the running binary, made if
/// missing: two builds of one commit (a dirty tree) are told apart.
fn make_copy() -> Result<PathBuf, String> {
    let source = std::env::current_exe().map_err(|e| format!("current exe: {e}"))?;
    let meta = fs::metadata(&source).map_err(|e| format!("{}: {e}", source.display()))?;
    let stamp = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let builds = builds_dir()?;
    let key = builds.join(format!("{}-{}-{stamp}", build_id(), meta.len()));
    let target = key.join("shux");
    if fs::metadata(&target).is_ok_and(|m| m.is_file() && m.len() == meta.len()) {
        touch_path(&key).map_err(|e| format!("{}: {e}", key.display()))?;
        return Ok(target);
    }
    let staging = builds.join(format!(".new-{}", std::process::id()));
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    let staged = staging.join("shux");
    let placed = place(&source, &staged).and_then(|()| {
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o755))?;
        fs::rename(&staging, &key)
    });
    match placed {
        Ok(()) => Ok(target),
        // Another daemon of this build published it first.
        Err(_) if fs::metadata(&target).is_ok_and(|m| m.len() == meta.len()) => {
            let _ = fs::remove_dir_all(&staging);
            Ok(target)
        }
        Err(e) => {
            let _ = fs::remove_dir_all(&staging);
            Err(format!(
                "copy {} to {}: {e}",
                source.display(),
                key.display()
            ))
        }
    }
}

/// A hard link where it is safe, a copy otherwise.
fn place(source: &Path, staged: &Path) -> std::io::Result<()> {
    // macOS lets a running binary be rewritten in place, which a link would
    // see; `copy` clones it on APFS, at no cost.
    if cfg!(target_os = "linux") && fs::hard_link(source, staged).is_ok() {
        return Ok(());
    }
    fs::copy(source, staged)?;
    fs::File::open(staged)?.sync_all()
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

/// Write this daemon's build file. Called once it holds the instance lock.
pub(crate) fn write_build_file(socket: &Path) {
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
pub(crate) fn handed_to() -> Option<String> {
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
}
