//! The share tunnel's supervisor, run on its own (#297).
//!
//! A stand-in for cloudflared writes one 256 MiB log line, then the startup
//! record. The supervisor must skip the line without holding it: its peak
//! memory stays far below the line's size, and the record after it is
//! still read.

use std::io::{BufRead, BufReader, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::process::{Command, Stdio};

const LINE: u64 = 256 << 20;

#[test]
fn an_overlong_log_line_is_skipped_without_being_held() {
    let dir = tempfile::tempdir().expect("dir");
    let fake = dir.path().join("cloudflared");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\n/usr/bin/head -c {LINE} /dev/zero | /usr/bin/tr '\\0' x >&2\n\
             printf '\\n' >&2\n\
             echo 'INF |  Your quick Tunnel has been created! Visit it at:  |' >&2\n\
             echo 'INF |  https://long-line.trycloudflare.com  |' >&2\n\
             exec /bin/sleep 3600.297\n"
        ),
    )
    .expect("write the stand-in");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let held: OwnedFd = listener.into();
    let raw = held.as_raw_fd();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_shux"));
    cmd.args(["__share-supervise", "--cloudflared"])
        .arg(&fake)
        .args(["--port", &port.to_string()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // The listener, where the supervisor takes it from.
        .stderr(Stdio::from(held.try_clone().expect("dup")));
    // SAFETY: dup2 or fcntl only, both async-signal-safe, between fork and exec; `held` outlives the spawn.
    // Older supervisors took the listener from fd 3 instead.
    unsafe {
        std::os::unix::process::CommandExt::pre_exec(&mut cmd, move || {
            let r = if raw == 3 {
                libc::fcntl(3, libc::F_SETFD, 0)
            } else {
                libc::dup2(raw, 3)
            };
            if r == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut sup = cmd.spawn().expect("spawn the supervisor");
    let mut lines = BufReader::new(sup.stdout.take().expect("stdout")).lines();
    let origin = lines
        .by_ref()
        .map_while(Result::ok)
        .find(|l| l.starts_with("origin ") || l.starts_with("exit "))
        .expect("a report");
    assert_eq!(origin, "origin https://long-line.trycloudflare.com");
    // Stop it, and let it reap the stand-in.
    let mut stdin = sup.stdin.take().expect("stdin");
    let _ = stdin.write_all(b"x");
    drop(stdin);
    assert!(sup.wait().expect("wait").success());
    drop(held);

    // SAFETY: getrusage fills the struct it is given and nothing else.
    let peak = unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        assert_eq!(libc::getrusage(libc::RUSAGE_CHILDREN, &mut usage), 0);
        usage.ru_maxrss as u64
    };
    // Bytes on macOS, KiB on Linux.
    let peak_bytes = if cfg!(target_os = "macos") {
        peak
    } else {
        peak * 1024
    };
    assert!(
        peak_bytes < 64 << 20,
        "the supervisor held {} MiB for a {} MiB line",
        peak_bytes >> 20,
        LINE >> 20
    );
}
