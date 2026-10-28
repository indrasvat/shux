//! A signal that ends `shux session attach` must hand the user back the
//! terminal it found: cooked mode, echo, the normal screen.
//!
//! Drives the real binary on a PTY that is the client's CONTROLLING terminal.
//! That part is load-bearing: crossterm enters raw mode through `/dev/tty`, not
//! through stdin, so a PTY that is merely the child's stdio leaves raw mode
//! applied to whatever terminal the test runner owns.

use std::fs::File;
use std::io::Read;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::sys::termios::{LocalFlags, Termios, tcgetattr};
use nix::unistd::Pid;

struct Harness {
    bin: PathBuf,
    runtime: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        Self {
            bin: PathBuf::from(env!("CARGO_BIN_EXE_shux")),
            runtime: tempfile::tempdir().expect("temp runtime dir"),
        }
    }

    fn runtime_dir(&self) -> &Path {
        self.runtime.path()
    }

    fn create_session(&self, name: &str) {
        let params = serde_json::json!({ "name": name, "command": workload() });
        let out = Command::new(&self.bin)
            .env("XDG_RUNTIME_DIR", self.runtime_dir())
            .env("SHELL", "/bin/sh")
            .env_remove("SHUX")
            .args(["--format", "json", "rpc", "call", "session.create"])
            .args(["--params", &params.to_string()])
            .output()
            .expect("spawn shux rpc");
        assert!(
            out.status.success(),
            "session.create failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn kill_session(&self, name: &str) {
        let out = Command::new(&self.bin)
            .env("XDG_RUNTIME_DIR", self.runtime_dir())
            .env_remove("SHUX")
            .args(["session", "kill", name])
            .output()
            .expect("spawn shux session kill");
        assert!(
            out.status.success(),
            "session kill failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let pid: Option<i32> = std::fs::read_to_string(self.runtime_dir().join("shux/shux.pid"))
            .ok()
            .and_then(|s| s.trim().parse().ok());
        // By pidfile, never by an argv substring.
        if let Some(pid) = pid {
            let _ = kill(Pid::from_raw(pid), Signal::SIGTERM);
            for _ in 0..50 {
                if kill(Pid::from_raw(pid), None).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            panic!("daemon {pid} survived SIGTERM");
        }
    }
}

/// The three colour classes, then hold the pane open.
fn workload() -> String {
    "printf '\\033[38;2;120;220;180mTRUECOLOR\\033[0m \\033[38;5;208mINDEXED\\033[0m \
     \\033[34mBASIC\\033[0m\\n'; while :; do sleep 3600; done"
        .to_string()
}

/// An attach client whose controlling terminal is a PTY the test owns.
struct AttachedClient {
    child: Child,
    master: File,
    /// Held so the line discipline outlives the child and can be inspected.
    slave: OwnedFd,
    seen: Vec<u8>,
}

impl AttachedClient {
    fn spawn(h: &Harness, session: &str) -> Self {
        let size = nix::pty::Winsize {
            ws_row: 30,
            ws_col: 100,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = nix::pty::openpty(Some(&size), None).expect("openpty");
        let dup = |fd: &OwnedFd| Stdio::from(fd.try_clone().expect("dup slave"));
        let mut cmd = Command::new(&h.bin);
        cmd.env("XDG_RUNTIME_DIR", h.runtime_dir())
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env_remove("NO_COLOR")
            .env_remove("SHUX")
            .args(["session", "attach", session])
            .stdin(dup(&pty.slave))
            .stdout(dup(&pty.slave))
            .stderr(dup(&pty.slave));
        // SAFETY: only async-signal-safe calls between fork and exec. A new
        // session plus TIOCSCTTY makes the PTY the child's controlling
        // terminal, so `/dev/tty` resolves to it and not to the runner's.
        unsafe {
            cmd.pre_exec(|| {
                nix::unistd::setsid()?;
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                // SIGQUIT's default action dumps core; keep it out of the tree.
                let no_core = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                if libc::setrlimit(libc::RLIMIT_CORE, &no_core) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().expect("spawn attach");
        nix::fcntl::fcntl(
            &pty.master,
            nix::fcntl::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
        )
        .expect("set O_NONBLOCK");
        Self {
            child,
            master: File::from(pty.master),
            slave: pty.slave,
            seen: Vec::new(),
        }
    }

    /// Drain whatever the client has written. Keeps the PTY from filling and
    /// blocking the client mid-write.
    fn pump(&mut self) {
        let mut buf = [0u8; 8192];
        while let Ok(n) = self.master.read(&mut buf) {
            if n == 0 {
                break;
            }
            self.seen.extend_from_slice(&buf[..n]);
        }
    }

    fn termios(&self) -> Termios {
        tcgetattr(self.slave.as_fd()).expect("tcgetattr slave")
    }

    /// Wait until the pane's colours have been drawn: the client is past the
    /// handshake, in raw mode, and inside its loop.
    fn wait_until_drawing(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            self.pump();
            if find(&self.seen, b"38;2;120;220;180") && find(&self.seen, b"38;5;208") {
                return;
            }
            if let Ok(Some(status)) = self.child.try_wait() {
                panic!(
                    "attach exited ({status}) before drawing: {:?}",
                    String::from_utf8_lossy(&self.seen)
                );
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!(
            "attach never drew the pane: {:?}",
            String::from_utf8_lossy(&self.seen)
        );
    }

    fn wait_for_exit(&mut self) -> std::process::ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            self.pump();
            if let Ok(Some(status)) = self.child.try_wait() {
                self.pump();
                return status;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let tail = &self.seen[self.seen.len().saturating_sub(400)..];
        panic!(
            "attach client still running after 10s; last output: {:?}",
            String::from_utf8_lossy(tail)
        );
    }
}

impl Drop for AttachedClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).rposition(|w| w == needle)
}

fn assert_signal_restores_terminal(sig: Signal) {
    let h = Harness::new();
    let session = format!("sig-{}", sig.as_str().to_ascii_lowercase());
    h.create_session(&session);

    let mut client = AttachedClient::spawn(&h, &session);
    let before = client.termios();
    assert!(
        before
            .local_flags
            .contains(LocalFlags::ECHO | LocalFlags::ICANON),
        "a fresh PTY should start cooked: {:?}",
        before.local_flags
    );

    client.wait_until_drawing();
    let raw = client.termios();
    assert!(
        !raw.local_flags
            .intersects(LocalFlags::ECHO | LocalFlags::ICANON),
        "precondition: the client is not in raw mode, so this test proves nothing: {:?}",
        raw.local_flags
    );
    let text = String::from_utf8_lossy(&client.seen).into_owned();
    assert!(text.contains("38;2;120;220;180"), "truecolor probe missing");
    assert!(text.contains("38;5;208"), "indexed probe missing");
    // The compositor re-encodes SGR 34 as palette slot 4.
    assert!(text.contains("38;5;4m"), "basic colour probe missing");
    let drawn = client.seen.len();

    kill(Pid::from_raw(client.child.id() as i32), sig).expect("signal attach");
    let status = client.wait_for_exit();

    let after = client.termios();
    assert_eq!(
        after.local_flags, before.local_flags,
        "{sig}: local flags not restored (ECHO/ICANON/ISIG)"
    );
    assert_eq!(after.input_flags, before.input_flags, "{sig}: input flags");
    assert_eq!(
        after.output_flags, before.output_flags,
        "{sig}: output flags"
    );
    assert_eq!(
        after.control_flags, before.control_flags,
        "{sig}: control flags"
    );

    let tail = &client.seen[drawn..];
    assert!(
        rfind(tail, b"\x1b[?1049l").is_some(),
        "{sig}: never left the alternate screen after the signal: {:?}",
        String::from_utf8_lossy(tail)
    );

    // The signal is re-raised once the terminal is restored, so a parent shell
    // sees the death it caused rather than a clean exit.
    assert_eq!(
        status.signal(),
        Some(sig as i32),
        "{sig}: attach should die by the signal it was sent, got {status}"
    );
}

#[test]
fn sigterm_restores_the_terminal() {
    assert_signal_restores_terminal(Signal::SIGTERM);
}

#[test]
fn sigint_restores_the_terminal() {
    assert_signal_restores_terminal(Signal::SIGINT);
}

#[test]
fn sighup_restores_the_terminal() {
    assert_signal_restores_terminal(Signal::SIGHUP);
}

#[test]
fn sigquit_restores_the_terminal() {
    assert_signal_restores_terminal(Signal::SIGQUIT);
}

/// The other way a client's session goes away under it: the session is killed.
#[test]
fn a_killed_session_ends_the_client_and_restores_the_terminal() {
    let h = Harness::new();
    h.create_session("sig-session-kill");
    let mut client = AttachedClient::spawn(&h, "sig-session-kill");
    let before = client.termios();
    client.wait_until_drawing();

    h.kill_session("sig-session-kill");
    let status = client.wait_for_exit();

    assert!(
        status.success(),
        "attach exited {status} after session kill"
    );
    let after = client.termios();
    assert_eq!(
        after.local_flags, before.local_flags,
        "local flags not restored"
    );
    let text = String::from_utf8_lossy(&client.seen);
    assert!(
        text.contains("[session 'sig-session-kill' ended]"),
        "the client never said why it left: {text:?}"
    );
}
