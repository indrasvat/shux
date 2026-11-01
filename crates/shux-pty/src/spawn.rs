//! Spawn children that inherit nothing but their stdio.
//!
//! The daemon is multithreaded and, outside Linux, every pipe, socket and PTY
//! pair it opens becomes close-on-exec only in a second step (`std`'s pipe,
//! `openpty`, `accept`). A child forked in between inherits whatever another
//! thread just opened: another pane's terminal, a client's socket, a plugin's
//! pipe. [`inherit_only_stdio`] marks every descriptor above stderr
//! close-on-exec between fork and exec.

use std::process::Command;

use nix::libc;

/// Make `cmd`'s child inherit nothing but stdio. Uses `pre_exec`, so `std`
/// forks rather than `posix_spawn`s.
pub fn inherit_only_stdio(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    let mut sweep = Sweep::prepare();
    // SAFETY: `Sweep::apply` makes only async-signal-safe calls into memory
    // allocated here, before the fork.
    unsafe {
        cmd.pre_exec(move || {
            sweep.apply();
            Ok(())
        });
    }
}

/// What the child needs to find its descriptors without allocating.
struct Sweep {
    /// Highest descriptor number the fallback loop reaches: `getdtablesize`.
    /// shux never lowers its own descriptor limit, so none sits above it.
    ceiling: i32,
    #[cfg(target_os = "macos")]
    listed: Vec<libc::proc_fdinfo>,
}

impl Sweep {
    fn prepare() -> Self {
        // SAFETY: takes no arguments and only reads a limit.
        let ceiling = unsafe { libc::getdtablesize() };
        Self {
            ceiling,
            // Room for everything open now plus whatever other threads open
            // before the fork; a fuller table falls back to the loop.
            #[cfg(target_os = "macos")]
            listed: vec![
                libc::proc_fdinfo {
                    proc_fd: 0,
                    proc_fdtype: 0
                };
                open_count().saturating_mul(2).saturating_add(256)
            ],
        }
    }

    /// Between fork and exec only.
    fn apply(&mut self) {
        #[cfg(target_os = "linux")]
        // SAFETY: close_range takes plain integers, and CLOEXEC closes nothing.
        if unsafe {
            libc::syscall(
                libc::SYS_close_range,
                3u32,
                u32::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            )
        } == 0
        {
            return;
        }
        #[cfg(target_os = "macos")]
        if let Some(fds) = self.list() {
            fds.iter().for_each(|info| mark(info.proc_fd));
            return;
        }
        (3..self.ceiling).for_each(mark);
    }

    /// This process's open descriptors, or `None` if the table may not fit.
    #[cfg(target_os = "macos")]
    fn list(&mut self) -> Option<&[libc::proc_fdinfo]> {
        let size = std::mem::size_of::<libc::proc_fdinfo>();
        let bytes = i32::try_from(self.listed.len() * size).ok()?;
        // SAFETY: the buffer is `bytes` long and owned by `self`; the call
        // only writes into it.
        let got = unsafe {
            libc::proc_pidinfo(
                libc::getpid(),
                libc::PROC_PIDLISTFDS,
                0,
                self.listed.as_mut_ptr().cast(),
                bytes,
            )
        };
        // A full buffer may have been truncated.
        if got <= 0 || got >= bytes {
            return None;
        }
        Some(&self.listed[..got as usize / size])
    }
}

/// Descriptors open in this process now, to size the child's listing.
#[cfg(target_os = "macos")]
fn open_count() -> usize {
    // SAFETY: a null buffer asks only for the size the listing needs.
    let bytes = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDLISTFDS,
            0,
            std::ptr::null_mut(),
            0,
        )
    };
    usize::try_from(bytes).unwrap_or(0) / std::mem::size_of::<libc::proc_fdinfo>()
}

/// Mark `fd` close-on-exec if it is open and above stderr.
fn mark(fd: i32) {
    if fd < 3 {
        return;
    }
    // SAFETY: F_GETFD/F_SETFD on any integer is defined: EBADF at worst.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags >= 0 && flags & libc::FD_CLOEXEC == 0 {
            libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    /// A pipe made without close-on-exec stands in for one another thread
    /// opened a moment before the fork. `/bin/test` opens nothing that high,
    /// so `-e /dev/fd/N` is true only if the child inherited it.
    fn inherits(fd: i32, hygiene: bool) -> bool {
        let mut cmd = Command::new("/bin/test");
        cmd.args(["-e", &format!("/dev/fd/{fd}")]);
        if hygiene {
            inherit_only_stdio(&mut cmd);
        }
        cmd.status().expect("run /bin/test").success()
    }

    #[test]
    fn a_child_inherits_no_descriptor_beyond_its_stdio() {
        let (_read, write) = nix::unistd::pipe().expect("pipe");
        let fd = write.as_raw_fd();
        assert!(inherits(fd, false), "the probe cannot see fd {fd}");
        assert!(!inherits(fd, true), "the child inherited fd {fd}");
    }

    /// The sweep marks rather than closes, so `std`'s exec-error pipe still
    /// reports a program that does not exist.
    #[test]
    fn a_missing_program_is_still_an_error() {
        let mut cmd = Command::new("/nonexistent/shux-spawn-probe");
        inherit_only_stdio(&mut cmd);
        let err = cmd.status().expect_err("spawning a missing program");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    /// The listing path sized for a table that does not fit must still mark
    /// everything, through the loop.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_table_too_big_for_the_listing_falls_back_to_the_loop() {
        let (_read, write) = nix::unistd::pipe().expect("pipe");
        let fd = write.as_raw_fd();
        let mut sweep = Sweep::prepare();
        sweep.listed.truncate(1);
        assert!(
            sweep.list().is_none(),
            "a one-entry buffer cannot hold the table"
        );
        let mut cmd = Command::new("/bin/test");
        cmd.args(["-e", &format!("/dev/fd/{fd}")]);
        use std::os::unix::process::CommandExt;
        // SAFETY: as in `inherit_only_stdio`.
        unsafe {
            cmd.pre_exec(move || {
                sweep.apply();
                Ok(())
            });
        }
        assert!(!cmd.status().expect("run /bin/test").success());
    }
}
