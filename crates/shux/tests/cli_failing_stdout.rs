use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, Output, Stdio};

struct Daemon {
    runtime: tempfile::TempDir,
}

impl Daemon {
    fn new() -> Self {
        Self {
            runtime: tempfile::tempdir().expect("runtime dir"),
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_shux"));
        command
            .env("XDG_RUNTIME_DIR", self.runtime.path())
            .env_remove("SHUX")
            .env_remove("SHUX_SOCKET");
        command
    }

    /// Starts the daemon with normal limits: one forked under the client's
    /// zero file-size limit could not write its own pidfile.
    fn start(&self) {
        let out = self
            .command()
            .args(["--format", "json", "session", "list"])
            .output()
            .expect("start daemon");
        assert!(out.status.success(), "{out:?}");
    }

    /// Runs with stdout on a regular file that refuses every write (EFBIG):
    /// a stdout that fails with something other than a broken pipe.
    fn failing_stdout(&self, args: &[&str]) -> Output {
        let file = tempfile::tempfile().expect("stdout file");
        let mut command = self.command();
        command.args(args).stdout(file).stderr(Stdio::piped());
        // SAFETY: setrlimit and signal are async-signal-safe and only touch
        // the child. SIGXFSZ ignored makes an over-limit write fail with EFBIG
        // instead of killing the process.
        unsafe {
            command.pre_exec(|| {
                let none = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: libc::RLIM_INFINITY,
                };
                if libc::setrlimit(libc::RLIMIT_FSIZE, &none) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
                Ok(())
            });
        }
        command
            .spawn()
            .expect("spawn shux")
            .wait_with_output()
            .expect("wait for shux")
    }

    fn closed_stdout(&self, args: &[&str]) -> Output {
        let (read, write) = nix::unistd::pipe().expect("pipe");
        drop(read);
        self.command()
            .args(args)
            .stdout(Stdio::from(write))
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn shux")
            .wait_with_output()
            .expect("wait for shux")
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let pid = std::fs::read_to_string(self.runtime.path().join("shux/shux.pid"))
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok());
        if let Some(pid) = pid
            && !shux_testkit::terminate(pid)
            && !std::thread::panicking()
        {
            panic!("daemon {pid} survived SIGTERM");
        }
    }
}

/// Output that never arrived fails the command: exit 1, the cause on stderr,
/// and no panic.
fn assert_write_failed(output: Output) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked"), "{} {stderr}", output.status);
    assert_eq!(
        output.status.code(),
        Some(1),
        "a lost write reported {} stderr: {stderr}",
        output.status
    );
    assert!(
        stderr.contains("File too large"),
        "stderr does not name the write error: {stderr}"
    );
}

#[test]
fn failing_stdout_version() {
    let daemon = Daemon::new();
    assert_write_failed(daemon.failing_stdout(&["version"]));
}

#[test]
fn failing_stdout_version_plain() {
    let daemon = Daemon::new();
    assert_write_failed(daemon.failing_stdout(&["--format", "plain", "version"]));
}

#[test]
fn failing_stdout_version_json() {
    let daemon = Daemon::new();
    assert_write_failed(daemon.failing_stdout(&["--format", "json", "version"]));
}

#[test]
fn failing_stdout_session_list() {
    let daemon = Daemon::new();
    daemon.start();
    let made = daemon
        .command()
        .args(["session", "create", "listed", "-d"])
        .output()
        .expect("create session");
    assert!(made.status.success(), "{made:?}");
    assert_write_failed(daemon.failing_stdout(&["--format", "text", "session", "list"]));
}

#[test]
fn failing_stdout_success_line() {
    let daemon = Daemon::new();
    daemon.start();
    assert_write_failed(daemon.failing_stdout(&["session", "create", "made", "-d"]));
}

/// A helper's write to a closed pipe still ends the command by SIGPIPE.
#[test]
fn closed_stdout_version_subcommand() {
    let daemon = Daemon::new();
    let output = daemon.closed_stdout(&["version"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert_eq!(
        output.status.signal(),
        Some(13),
        "status {} stderr: {stderr}",
        output.status
    );
}
