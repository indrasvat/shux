use std::os::unix::process::ExitStatusExt;
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

/// A closed stdout pipe kills the client with SIGPIPE. There is no panic
/// text, and the shell's status for that death is 141 (128 + 13).
fn assert_sigpipe(output: Output) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked") && !stderr.contains("failed printing"),
        "closed stdout panicked ({}): {stderr}",
        output.status
    );
    assert_eq!(
        output.status.signal(),
        Some(13),
        "expected SIGPIPE, status {} stderr: {stderr}",
        output.status
    );
}

#[test]
fn closed_stdout_session_list() {
    let daemon = Daemon::new();
    assert_sigpipe(daemon.closed_stdout(&["--format", "json", "session", "list"]));
}

/// clap prints help and version itself and already ignores a closed pipe:
/// quiet, whether it exits 0 or by SIGPIPE.
fn assert_quiet(output: Output) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked") && !stderr.contains("failed printing"),
        "closed stdout panicked ({}): {stderr}",
        output.status
    );
    assert!(
        output.status.success() || output.status.signal() == Some(13),
        "status {} stderr: {stderr}",
        output.status
    );
}

#[test]
fn closed_stdout_help() {
    let daemon = Daemon::new();
    assert_quiet(daemon.closed_stdout(&["--help"]));
}

#[test]
fn closed_stdout_version() {
    let daemon = Daemon::new();
    assert_quiet(daemon.closed_stdout(&["--version"]));
}

#[test]
fn closed_stdout_preserves_argument_errors() {
    let daemon = Daemon::new();
    let output = daemon.closed_stdout(&["--not-a-shux-option"]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unexpected argument"), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
}

/// A missing session is still a failed command when stdout is the closed
/// pipe. The message goes to stderr, which is open, so the process exits 1
/// instead of dying by SIGPIPE or swallowing the error.
#[test]
fn closed_stdout_preserves_runtime_errors() {
    let daemon = Daemon::new();
    let output = daemon.closed_stdout(&["session", "kill", "--session", "missing-closed-stdout"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked") && !stderr.contains("failed printing"),
        "closed stdout panicked ({}): {stderr}",
        output.status
    );
    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(!stderr.trim().is_empty(), "error was swallowed: {stderr}");
}

/// A closed stderr: the error cannot be shown, but the command still fails,
/// by its status or by SIGPIPE; it does not exit 0, and it does not panic.
#[test]
fn closed_stderr_still_fails_the_command() {
    let daemon = Daemon::new();
    let (read, write) = nix::unistd::pipe().expect("pipe");
    drop(read);
    let output = daemon
        .command()
        .args(["session", "kill", "--session", "missing-closed-stderr"])
        .stderr(Stdio::from(write))
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn shux")
        .wait_with_output()
        .expect("wait for shux");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert!(
        output.status.code() == Some(1) || output.status.signal() == Some(13),
        "a failed command on a closed stderr reported {}",
        output.status
    );
}

#[tokio::test]
async fn closed_stdout_daemon_still_ignores_sigpipe() {
    let daemon = Daemon::new();
    let first = daemon
        .command()
        .args(["--format", "json", "session", "list"])
        .output()
        .expect("start daemon");
    assert!(first.status.success(), "{first:?}");
    let pidfile = daemon.runtime.path().join("shux/shux.pid");
    let pid = std::fs::read_to_string(&pidfile).expect("daemon pidfile");
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid.trim().parse().expect("daemon pid")),
        nix::sys::signal::Signal::SIGPIPE,
    )
    .expect("send daemon SIGPIPE");
    // Connect directly so auto-start cannot hide a daemon killed by SIGPIPE.
    let after = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut stream =
            tokio::net::UnixStream::connect(daemon.runtime.path().join("shux/shux.sock"))
                .await
                .expect("connect to existing daemon");
        shux::cli::rpc::rpc_call(&mut stream, "session.list", serde_json::json!({}))
            .await
            .expect("daemon response after SIGPIPE")
    })
    .await
    .expect("daemon must still respond");
    assert_eq!(
        after,
        serde_json::from_slice::<serde_json::Value>(&first.stdout).expect("session list JSON")
    );
    assert_eq!(
        std::fs::read_to_string(pidfile).expect("daemon pidfile"),
        pid
    );
}

/// Started with SIGPIPE blocked (an inherited mask): still no panic, and the
/// command still ends the way a closed pipe should.
#[test]
fn closed_stdout_with_sigpipe_blocked() {
    use std::os::unix::process::CommandExt;
    let daemon = Daemon::new();
    let (read, write) = nix::unistd::pipe().expect("pipe");
    drop(read);
    let mut command = daemon.command();
    command.args(["--format", "json", "session", "list"]);
    // SAFETY: sigprocmask is async-signal-safe and only touches the child.
    unsafe {
        command.pre_exec(|| {
            let mut pipe = nix::sys::signal::SigSet::empty();
            pipe.add(nix::sys::signal::Signal::SIGPIPE);
            pipe.thread_block().map_err(std::io::Error::from)
        });
    }
    let output = command
        .stdout(Stdio::from(write))
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn shux")
        .wait_with_output()
        .expect("wait for shux");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked"), "{} {stderr}", output.status);
    assert!(
        output.status.signal() == Some(13) || output.status.code() == Some(141),
        "status {} stderr: {stderr}",
        output.status
    );
}
