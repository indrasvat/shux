//! Where a daemon keeps its workspace between runs (issue #329):
//! `$XDG_STATE_HOME/shux/servers/<key>/`, one directory per socket.
//!
//! | File | |
//! |---|---|
//! | `lock` | held by the live daemon for its whole run; never unlinked |
//! | `owner` | that daemon's pid, so a contended successor can name it |
//! | `workspace.json` | the last durable workspace |
//! | `restoring` | present while a restore runs: how many have started |
//! | `poisoned` | a close could neither be saved nor the workspace deleted |
//! | `quarantine/` | workspaces a boot refused, kept as evidence |

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

const WORKSPACE: &str = "workspace.json";
const RESTORING: &str = "restoring";
const POISONED: &str = "poisoned";
const OWNER: &str = "owner";
const QUARANTINE: &str = "quarantine";

/// `$XDG_STATE_HOME/shux`, or `~/.local/state/shux`. An empty or relative
/// `XDG_STATE_HOME` is ignored, as the XDG spec says.
pub fn state_root() -> Option<PathBuf> {
    match std::env::var_os("XDG_STATE_HOME").map(PathBuf::from) {
        Some(p) if p.is_absolute() => Some(p.join("shux")),
        _ => std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|h| h.is_absolute())
            .map(|h| h.join(".local/state/shux")),
    }
}

/// The directory name for a socket: its canonical key, hashed. The pidfile
/// suffix is not used: it is empty for every runtime dir's default socket.
pub fn key_for(socket_key: &Path) -> String {
    let digest = Sha256::digest(socket_key.as_os_str().as_encoded_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// Open (creating if needed) a directory we own, refusing a symlink or a
/// directory someone else owns, and tighten it to 0700.
fn own_dir(path: &Path) -> io::Result<File> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let dir = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let owner = dir.metadata()?.uid();
    let me = nix::unistd::geteuid().as_raw();
    if owner != me {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is owned by uid {owner}, not {me}", path.display()),
        ));
    }
    // Only ever take permission away: a directory its owner made read-only
    // stays read-only, and is then refused as unwritable.
    let mode = dir.metadata()?.permissions().mode() & 0o7777;
    if mode & 0o077 != 0 {
        dir.set_permissions(fs::Permissions::from_mode(mode & 0o700))?;
    }
    Ok(dir)
}

fn open_regular(path: &Path, opts: &mut OpenOptions) -> io::Result<File> {
    let file = opts
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    Ok(file)
}

/// Quarantined workspaces kept as evidence; older ones are deleted, so a
/// workspace refused on every boot cannot fill the disk.
const QUARANTINE_KEEP: usize = 10;

fn prune_quarantine(q: &Path) -> io::Result<()> {
    let mut names: Vec<_> = fs::read_dir(q)?
        .filter_map(Result::ok)
        .map(|e| e.file_name())
        .collect();
    // Fixed-width names: oldest first once sorted.
    names.sort();
    let excess = names.len().saturating_sub(QUARANTINE_KEEP);
    for name in names.into_iter().take(excess) {
        fs::remove_file(q.join(name))?;
    }
    Ok(())
}

/// A small bookkeeping file (`owner`, `restoring`): a regular file, read no
/// further than 128 bytes, so a planted FIFO or `/dev/zero` cannot hang or
/// swamp a booting daemon. `None` when it is not there.
fn read_small(path: &Path) -> io::Result<Option<String>> {
    let file = match open_regular(path, OpenOptions::new().read(true)) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut s = String::new();
    file.take(128).read_to_string(&mut s)?;
    Ok(Some(s))
}

/// Why a daemon cannot use its state directory.
#[derive(Debug)]
pub enum OpenError {
    /// Another live daemon holds the lock; its pid when it wrote one.
    Held { holder: Option<u32> },
    /// The directory cannot be created, opened, locked or written.
    Unusable(io::Error),
}

/// A daemon's own state directory, locked for as long as this value lives.
#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
    dir_handle: File,
    /// The flock lives with this handle.
    _lock: File,
    /// Every workspace write fails, as on a full disk.
    #[cfg(test)]
    pub(crate) fail_writes: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Every removal fails, as in a directory gone read-only.
    #[cfg(test)]
    pub(crate) fail_removes: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Store {
    /// Take ownership of `root/servers/<key>`.
    pub fn open(root: &Path, key: &str) -> Result<Self, OpenError> {
        let unusable = OpenError::Unusable;
        if let Some(parent) = root.parent() {
            fs::create_dir_all(parent).map_err(unusable)?;
        }
        own_dir(root).map_err(unusable)?;
        own_dir(&root.join("servers")).map_err(unusable)?;
        let dir = root.join("servers").join(key);
        let dir_handle = own_dir(&dir).map_err(unusable)?;
        let lock = open_regular(
            &dir.join("lock"),
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600),
        )
        .map_err(unusable)?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                let holder = read_small(&dir.join(OWNER))
                    .ok()
                    .flatten()
                    .and_then(|s| s.trim().parse().ok());
                return Err(OpenError::Held { holder });
            }
            Err(fs::TryLockError::Error(e)) => return Err(unusable(e)),
        }
        let store = Self {
            dir,
            dir_handle,
            _lock: lock,
            #[cfg(test)]
            fail_writes: Default::default(),
            #[cfg(test)]
            fail_removes: Default::default(),
        };
        store
            .replace(OWNER, std::process::id().to_string().as_bytes())
            .map_err(unusable)?;
        Ok(store)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The saved workspace, or `None` when there is none. Refuses a file over
    /// `limit` before reading it.
    pub fn read_workspace(&self, limit: u64) -> io::Result<Option<Vec<u8>>> {
        let mut file = match open_regular(&self.dir.join(WORKSPACE), OpenOptions::new().read(true))
        {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let len = file.metadata()?.len();
        if len > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("workspace is {len} bytes, over {limit}"),
            ));
        }
        let mut bytes = Vec::with_capacity(len as usize);
        Read::by_ref(&mut file)
            .take(limit + 1)
            .read_to_end(&mut bytes)?;
        Ok(Some(bytes))
    }

    /// Replace the workspace durably: once this returns, a crash or power
    /// loss leaves either these bytes or nothing newer.
    pub fn write_workspace(&self, bytes: &[u8]) -> io::Result<()> {
        #[cfg(test)]
        if self.fail_writes.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(io::Error::from_raw_os_error(libc::ENOSPC));
        }
        self.replace(WORKSPACE, bytes)
    }

    /// Remove the workspace durably, so nothing is restored.
    pub fn remove_workspace(&self) -> io::Result<()> {
        self.remove(WORKSPACE)
    }

    /// The workspace a restore was under way for, and how many restores of it
    /// have started without finishing.
    pub fn restoring(&self) -> io::Result<Option<u32>> {
        Ok(read_small(&self.dir.join(RESTORING))?.map(|s| {
            // Unreadable counts as one: a restore began and did not finish.
            s.split_whitespace()
                .last()
                .and_then(|n| n.parse().ok())
                .unwrap_or(1)
        }))
    }

    pub fn mark_restoring(&self, attempts: u32) -> io::Result<()> {
        self.replace(RESTORING, attempts.to_string().as_bytes())
    }

    /// The workspace could be neither saved nor deleted after a close: the
    /// next start must not restore it.
    pub fn mark_poisoned(&self) -> io::Result<()> {
        self.replace(POISONED, b"1")
    }

    pub fn is_poisoned(&self) -> io::Result<bool> {
        Ok(read_small(&self.dir.join(POISONED))?.is_some())
    }

    pub fn clear_poisoned(&self) -> io::Result<()> {
        self.remove(POISONED)
    }

    pub fn clear_restoring(&self) -> io::Result<()> {
        self.remove(RESTORING)
    }

    /// Move the workspace aside as `quarantine/<nanos>-<n>-<reason>.json`.
    pub fn quarantine(&self, reason: &str) -> io::Result<PathBuf> {
        let q = self.dir.join(QUARANTINE);
        own_dir(&q)?;
        // Fixed-width nanoseconds: names sort in the order they were made.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let reason: String = reason
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .take(40)
            .collect();
        for n in 0..1000u32 {
            let to = q.join(format!("{nanos:020}-{n:03}-{reason}.json"));
            // `link` refuses an existing name, so two quarantines at one
            // instant never overwrite each other's evidence.
            match fs::hard_link(self.dir.join(WORKSPACE), &to) {
                Ok(()) => {
                    self.remove(WORKSPACE)?;
                    prune_quarantine(&q)?;
                    File::open(&q)?.sync_all()?;
                    return Ok(to);
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::other("no free quarantine name"))
    }

    /// Delete the workspace, the restore marker and everything else a run
    /// keeps, but not quarantined evidence.
    pub fn discard(&self) -> io::Result<()> {
        self.remove(WORKSPACE)?;
        self.remove(RESTORING)?;
        self.remove(POISONED)
    }

    fn replace(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        let tmp = self.dir.join(format!(".{name}.{}.tmp", std::process::id()));
        let _ = fs::remove_file(&tmp);
        let mut file = open_regular(
            &tmp,
            OpenOptions::new().write(true).create_new(true).mode(0o600),
        )?;
        let written = file.write_all(bytes).and_then(|()| file.sync_all());
        if let Err(e) = written {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        if let Err(e) = fs::rename(&tmp, self.dir.join(name)) {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        self.dir_handle.sync_all()
    }

    fn remove(&self, name: &str) -> io::Result<()> {
        #[cfg(test)]
        if self.fail_removes.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(io::Error::from_raw_os_error(libc::EACCES));
        }
        match fs::remove_file(self.dir.join(name)) {
            Ok(()) => self.dir_handle.sync_all(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> (tempfile::TempDir, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let r = t.path().join("state/shux");
        (t, r)
    }

    /// The lock excludes a second owner while the first lives, and only then.
    #[test]
    fn one_owner_at_a_time() {
        let (_t, r) = root();
        let first = Store::open(&r, "k").unwrap();
        match Store::open(&r, "k") {
            Err(OpenError::Held { holder }) => assert_eq!(holder, Some(std::process::id())),
            other => panic!("second owner admitted: {other:?}"),
        }
        drop(first);
        Store::open(&r, "k").unwrap();
    }

    /// A planted FIFO, or a link to an endless file, where the restore marker
    /// lives is refused at once: it cannot hang or swamp a booting daemon.
    #[test]
    fn a_planted_marker_neither_blocks_nor_floods() {
        let (_t, r) = root();
        let s = Store::open(&r, "k").unwrap();
        let marker = s.dir().join(RESTORING);
        nix::unistd::mkfifo(&marker, nix::sys::stat::Mode::S_IRWXU).unwrap();
        assert!(s.restoring().is_err(), "a FIFO was read as a marker");
        fs::remove_file(&marker).unwrap();
        std::os::unix::fs::symlink("/dev/zero", &marker).unwrap();
        assert!(s.restoring().is_err(), "a link was followed");
    }

    /// A state directory its owner made read-only stays read-only.
    #[test]
    fn opening_never_grants_write_permission() {
        let (_t, r) = root();
        let dir = Store::open(&r, "k").unwrap().dir().to_path_buf();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
        let _ = Store::open(&r, "k");
        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(mode, 0o500);
    }

    /// A planted symlink at the server directory is refused, not followed.
    #[test]
    fn a_symlinked_server_dir_is_refused() {
        let (t, r) = root();
        fs::create_dir_all(r.join("servers")).unwrap();
        let elsewhere = t.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, r.join("servers/k")).unwrap();
        assert!(matches!(Store::open(&r, "k"), Err(OpenError::Unusable(_))));
        assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
    }

    /// Quarantine keeps every refused file, even two in the same second.
    #[test]
    fn quarantine_never_overwrites_evidence() {
        let (_t, r) = root();
        let s = Store::open(&r, "k").unwrap();
        s.write_workspace(b"one").unwrap();
        let a = s.quarantine("bad").unwrap();
        s.write_workspace(b"two").unwrap();
        let b = s.quarantine("bad").unwrap();
        assert_ne!(a, b);
        assert_eq!(fs::read(a).unwrap(), b"one");
        assert_eq!(fs::read(&b).unwrap(), b"two");
        assert_eq!(s.read_workspace(1 << 20).unwrap(), None);

        // Kept: the newest QUARANTINE_KEEP, however many boots refused one.
        let mut last = b;
        for i in 0..QUARANTINE_KEEP + 5 {
            s.write_workspace(format!("n{i}").as_bytes()).unwrap();
            last = s.quarantine("bad").unwrap();
        }
        let kept = fs::read_dir(s.dir().join(QUARANTINE)).unwrap().count();
        assert_eq!(kept, QUARANTINE_KEEP);
        assert!(last.exists(), "the newest evidence was deleted");
    }

    #[test]
    fn files_and_dirs_are_private() {
        let (_t, r) = root();
        let s = Store::open(&r, "k").unwrap();
        s.write_workspace(b"{}").unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(s.dir()), 0o700);
        assert_eq!(mode(&r.join("servers")), 0o700);
        assert_eq!(mode(&s.dir().join(WORKSPACE)), 0o600);
    }

    #[test]
    fn an_oversized_workspace_is_refused_unread() {
        let (_t, r) = root();
        let s = Store::open(&r, "k").unwrap();
        s.write_workspace(&[b'x'; 64]).unwrap();
        assert_eq!(s.read_workspace(64).unwrap().unwrap().len(), 64);
        assert_eq!(
            s.read_workspace(63).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
