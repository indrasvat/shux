//! A Unix socket that keeps its path.
//!
//! The daemon holds its instance lock for as long as it runs, so while it does
//! no replacement can start: if its socket file is unlinked -- by a tmp cleaner,
//! or by a daemon from before the lock tidying up on its way out -- clients find
//! nothing there for good. So the file is rebound when it goes missing, and
//! removed on drop only while it is still this listener's.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UnixListener;
use tracing::warn;

/// How often a socket checks that its path still reaches it.
pub const CHECK_INTERVAL: Duration = Duration::from_millis(500);

/// Asked before each rebind: whether this process may still serve the path.
pub type RebindGate = Arc<dyn Fn() -> bool + Send + Sync>;

pub struct OwnedSocket {
    path: PathBuf,
    listener: UnixListener,
    id: (u64, u64),
}

/// Device and inode: what makes a socket file this listener's.
fn file_id(path: &Path) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path)?;
    Ok((meta.dev(), meta.ino()))
}

fn bind_at(path: &Path) -> io::Result<(UnixListener, (u64, u64))> {
    use nix::sys::stat::{FchmodatFlags, Mode, fchmodat};
    let listener = UnixListener::bind(path)?;
    // PRD §13.1. Never through a symlink: whoever can write the directory can
    // swap the name between the bind and the chmod.
    fchmodat(
        nix::fcntl::AT_FDCWD,
        path,
        Mode::S_IRWXU,
        FchmodatFlags::NoFollowSymlink,
    )?;
    Ok((listener, file_id(path)?))
}

/// Whether something listens at `path`. Never blocks: a listener that stopped
/// accepting fills its backlog, and a blocking connect would then wait on it
/// for good. A full backlog counts as live where the kernel says so (`EAGAIN`
/// on Linux); BSD kernels refuse it exactly as they refuse a dead socket, so
/// there it reads as stale.
fn is_served(path: &Path) -> bool {
    use nix::errno::Errno;
    use nix::fcntl::{FcntlArg, OFlag, fcntl};
    use nix::sys::socket::{AddressFamily, SockFlag, SockType, UnixAddr, connect, socket};
    use std::os::fd::AsRawFd;
    let Ok(addr) = UnixAddr::new(path) else {
        return false;
    };
    let Ok(fd) = socket(
        AddressFamily::Unix,
        SockType::Stream,
        SockFlag::empty(),
        None,
    ) else {
        return false;
    };
    if fcntl(&fd, FcntlArg::F_SETFL(OFlag::O_NONBLOCK)).is_err() {
        return false;
    }
    matches!(
        connect(fd.as_raw_fd(), &addr),
        Ok(()) | Err(Errno::EAGAIN | Errno::EINPROGRESS)
    )
}

/// Whether nobody else can put a file at a name in `path`'s directory: it is
/// not a symlink, and is ours and unwritable by others, or sticky and ours or
/// root's. A sticky directory's owner can still unlink and replace any name in
/// it, so one owned by another user is not private.
fn parent_is_private(path: &Path) -> bool {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let Ok(dir) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
        .open(parent)
    else {
        return false;
    };
    dir.metadata().is_ok_and(|meta| {
        let mine = meta.uid() == nix::unistd::geteuid().as_raw();
        let sticky = meta.mode() & 0o1000 != 0;
        (mine && meta.mode() & 0o022 == 0) || (sticky && (mine || meta.uid() == 0))
    })
}

impl OwnedSocket {
    /// Bind `path`, replacing a stale file there but never a live socket.
    pub fn bind(path: &Path) -> io::Result<Self> {
        if is_served(path) {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("{} is already being served", path.display()),
            ));
        }
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let (listener, id) = bind_at(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            listener,
            id,
        })
    }

    pub fn listener(&self) -> &UnixListener {
        &self.listener
    }

    /// Rebind if the path is missing and `gate` allows it. Errors once another
    /// file holds the path: clients reach that, never this listener again.
    pub fn check(&mut self, gate: Option<&RebindGate>) -> io::Result<()> {
        match file_id(&self.path) {
            Ok(id) if id == self.id => return Ok(()),
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("{} now belongs to another listener", self.path.display()),
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Ok(()),
        }
        if gate.is_some_and(|may| !may()) {
            return Ok(());
        }
        if !parent_is_private(&self.path) {
            warn!(path = %self.path.display(), "socket was unlinked; its directory is not private, not rebinding");
            return Ok(());
        }
        match bind_at(&self.path) {
            Ok((listener, id)) => {
                warn!(path = %self.path.display(), "socket was unlinked; rebound it");
                self.listener = listener;
                self.id = id;
            }
            Err(e) => {
                warn!(path = %self.path.display(), error = %e, "socket was unlinked; rebind failed")
            }
        }
        Ok(())
    }
}

impl Drop for OwnedSocket {
    fn drop(&mut self) {
        if file_id(&self.path).is_ok_and(|id| id == self.id) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::{AsRawFd, OwnedFd};

    /// Connections queued on `path` until its listener's backlog refuses more.
    fn fill_backlog(path: &Path) -> Vec<OwnedFd> {
        use nix::sys::socket::{AddressFamily, SockFlag, SockType, UnixAddr, connect, socket};
        let addr = UnixAddr::new(path).expect("socket address");
        let mut queued = Vec::new();
        loop {
            let fd = socket(
                AddressFamily::Unix,
                SockType::Stream,
                SockFlag::empty(),
                None,
            )
            .expect("socket");
            nix::fcntl::fcntl(
                &fd,
                nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
            )
            .expect("nonblocking");
            match connect(fd.as_raw_fd(), &addr) {
                Ok(()) => queued.push(fd),
                Err(nix::errno::Errno::EAGAIN) => return queued,
                #[cfg(not(target_os = "linux"))]
                Err(nix::errno::Errno::ECONNREFUSED) if !queued.is_empty() => return queued,
                Err(e) => panic!("connect: {e}"),
            }
            assert!(queued.len() < 100_000, "the backlog never filled");
        }
    }

    /// A listener that stopped accepting is still live, and finding that out
    /// must not wedge the caller on its full queue.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_listener_with_a_full_backlog_is_occupied_not_waited_on() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("s.sock");
        let held = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        let _queued = fill_backlog(&path);
        let probe = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::task::spawn_blocking({
                let path = path.clone();
                let rt = tokio::runtime::Handle::current();
                move || {
                    let _rt = rt.enter();
                    OwnedSocket::bind(&path).map(|_| ())
                }
            })
            .await
        })
        .await;
        drop(held);
        let result = probe
            .expect("bind blocked on a listener with a full backlog")
            .expect("probe task");
        #[cfg(target_os = "linux")]
        assert_eq!(
            result.expect_err("bound over a live listener").kind(),
            io::ErrorKind::AddrInUse
        );
        #[cfg(not(target_os = "linux"))]
        let _ = result;
    }

    /// A sticky directory's owner can replace any name in it, so a rebind
    /// there is only safe when that owner is us or root. Needs root, to hand
    /// the directory to another user.
    #[test]
    fn a_sticky_directory_another_user_owns_is_not_private() {
        use std::os::unix::fs::PermissionsExt;
        if !nix::unistd::geteuid().is_root() {
            eprintln!("skipped: chown to another user needs root");
            return;
        }
        let dir = tempfile::tempdir().expect("temp dir");
        let shared = dir.path().join("shared");
        std::fs::create_dir(&shared).expect("mkdir");
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o1777)).expect("chmod");
        assert!(
            parent_is_private(&shared.join("s.sock")),
            "a sticky dir of ours"
        );
        std::os::unix::fs::chown(&shared, Some(65534), None).expect("chown");
        assert!(!parent_is_private(&shared.join("s.sock")));
    }

    /// What `bind_at` relies on: its chmod fails on a symlink rather than
    /// changing the file the link points at.
    #[test]
    fn the_socket_chmod_does_not_follow_a_symlink() {
        use nix::sys::stat::{FchmodatFlags, Mode, fchmodat};
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("temp dir");
        let target = dir.path().join("target");
        std::fs::write(&target, "").expect("target");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        let link = dir.path().join("s.sock");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let chmod = fchmodat(
            nix::fcntl::AT_FDCWD,
            &link,
            Mode::S_IRWXU,
            FchmodatFlags::NoFollowSymlink,
        );
        let mode = std::fs::metadata(&target)
            .expect("stat")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o644,
            "the chmod reached the link's target ({chmod:?})"
        );
    }

    #[tokio::test]
    async fn a_stale_socket_file_is_replaced() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("s.sock");
        drop(std::os::unix::net::UnixListener::bind(&path).expect("bind"));
        assert!(path.exists(), "the stale file is the precondition");
        let socket = OwnedSocket::bind(&path).expect("replace the stale socket");
        assert!(std::os::unix::net::UnixStream::connect(&path).is_ok());
        drop(socket);
        assert!(!path.exists(), "drop leaves its own socket behind");
    }
}
