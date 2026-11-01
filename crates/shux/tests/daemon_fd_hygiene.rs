//! A daemon started by a client keeps none of that client's descriptors.
//!
//! The client re-executes itself as the daemon, so anything its own starter
//! left open without close-on-exec (make's jobserver pipe, a CI step's output
//! pipe) would otherwise stay open for the daemon's life, and in every pane.

use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::Command;

struct Daemon {
    runtime: tempfile::TempDir,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let pid: Option<i32> = std::fs::read_to_string(self.runtime.path().join("shux/shux.pid"))
            .ok()
            .and_then(|s| s.trim().parse().ok());
        // By pidfile, never by an argv substring.
        if let Some(pid) = pid
            && !shux_testkit::terminate(pid)
            && !std::thread::panicking()
        {
            panic!("daemon {pid} survived SIGTERM");
        }
    }
}

#[test]
fn an_auto_started_daemon_holds_no_descriptor_its_starter_leaked() {
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_shux"));
    let daemon = Daemon {
        runtime: tempfile::tempdir().expect("runtime dir"),
    };
    // No close-on-exec: the client inherits the write end, as it would from a
    // starter that left one open.
    let (read, write) = nix::unistd::pipe().expect("pipe");
    let out = Command::new(&bin)
        .env("XDG_RUNTIME_DIR", daemon.runtime.path())
        .env("SHELL", "/bin/sh")
        .env_remove("SHUX")
        .args(["session", "create", "fd", "-d", "--", "sh", "-c"])
        .arg("printf '\\033[38;2;120;220;180mT\\033[38;5;208mI\\033[34mB\\033[0m\\n'; sleep 3600")
        .output()
        .expect("spawn shux");
    assert!(
        out.status.success(),
        "session create failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let fd = write.as_raw_fd();
    drop(write);

    let closed = shux_testkit::pipe_closes(read);
    let daemon_up = Command::new(&bin)
        .env("XDG_RUNTIME_DIR", daemon.runtime.path())
        .args(["--format", "json", "daemon", "status"])
        .output()
        .expect("daemon status");
    assert!(
        String::from_utf8_lossy(&daemon_up.stdout).contains("\"running\": true"),
        "the daemon is not running, so the pipe proves nothing: {}",
        String::from_utf8_lossy(&daemon_up.stdout)
    );
    assert!(
        closed,
        "fd {fd} never reached EOF: the daemon (or a pane) still holds its starter's pipe"
    );
}
