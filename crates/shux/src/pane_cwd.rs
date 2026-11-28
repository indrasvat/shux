//! Where a pane's shell is now (issue #329): the directory a restored pane
//! starts in. It is the shell process's own cwd; a nested shell or a remote
//! `ssh` session does not change it.

use std::path::PathBuf;

use crate::lens_scratch::process_start_token;

/// `pid`'s cwd, but only while `pid` is still the process that started at
/// `start_token`: a pid freed and reused between two reads is never sampled.
pub fn sample(pid: u32, start_token: u64) -> Option<PathBuf> {
    if process_start_token(pid)? != start_token {
        return None;
    }
    let cwd = read_cwd(pid)?;
    (process_start_token(pid)? == start_token).then_some(cwd)
}

#[cfg(target_os = "linux")]
fn read_cwd(pid: u32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
    // A removed directory reads as "<path> (deleted)": there is nowhere to go back to.
    if path.as_os_str().as_bytes().ends_with(b" (deleted)") {
        return None;
    }
    Some(path)
}

#[cfg(target_os = "macos")]
fn read_cwd(pid: u32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    // SAFETY: proc_pidinfo writes at most `size` bytes into `info`, a zeroed,
    // aligned proc_vnodepathinfo we own.
    let ret = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            (&raw mut info).cast::<libc::c_void>(),
            size,
        )
    };
    if ret != size {
        return None;
    }
    // SAFETY: `vip_path` is MAXPATHLEN contiguous c_chars inside `info`.
    let raw: &[u8] = unsafe {
        std::slice::from_raw_parts(
            info.pvi_cdir.vip_path.as_ptr().cast::<u8>(),
            std::mem::size_of_val(&info.pvi_cdir.vip_path),
        )
    };
    let end = raw.iter().position(|&b| b == 0)?;
    (end > 0).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(&raw[..end])))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn read_cwd(_pid: u32) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};

    /// A real shell that `cd`s is sampled at its new directory, not where it
    /// started; a wrong start token reads nothing.
    #[test]
    fn a_shell_is_sampled_where_it_has_gone() {
        let start = tempfile::tempdir().unwrap();
        let moved = tempfile::tempdir().unwrap();
        let mut child = Command::new("/bin/sh")
            .current_dir(start.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let mut out = BufReader::new(child.stdout.take().unwrap());
        writeln!(stdin, "cd '{}' && echo moved", moved.path().display()).unwrap();
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        assert_eq!(line.trim(), "moved");

        let pid = child.id();
        let token = process_start_token(pid).unwrap();
        let canonical = |p: &std::path::Path| p.canonicalize().unwrap();
        assert_eq!(
            sample(pid, token).map(|p| canonical(&p)),
            Some(canonical(moved.path()))
        );
        assert_eq!(sample(pid, token.wrapping_add(1)), None);

        // A removed directory is no place to come back to.
        drop(moved);
        #[cfg(target_os = "linux")]
        assert_eq!(sample(pid, token), None);

        drop(stdin);
        child.wait().unwrap();
        assert_eq!(sample(pid, token), None);
    }
}
