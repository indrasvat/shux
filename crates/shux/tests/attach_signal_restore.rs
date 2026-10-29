//! A signal that ends `shux session attach` must hand the user back the
//! terminal it found: cooked mode, echo, the normal screen.
//!
//! Drives the real binary on a PTY that is the client's CONTROLLING terminal.
//! That part is load-bearing: crossterm enters raw mode through `/dev/tty`, not
//! through stdin, so a PTY that is merely the child's stdio leaves raw mode
//! applied to whatever terminal the test runner owns.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use nix::sys::signal::{Signal, kill};
use nix::sys::termios::{LocalFlags, Termios, tcgetattr};
use nix::unistd::Pid;
use shux_rpc::attach::{
    ATTACH_PROTOCOL_VERSION, AttachClientFrame, AttachHello, AttachReady, AttachServerFrame,
};
use shux_testkit::{BUDGET, poll, poll_until};
use tokio::net::UnixStream;
use tokio_util::codec::Framed;

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
        self.create_session_running(name, &workload());
    }

    fn create_session_running(&self, name: &str, command: &str) {
        let params = serde_json::json!({ "name": name, "command": command });
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
            "session.create failed ({}): stdout {:?} stderr {:?}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
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
        if let Some(pid) = pid
            && !shux_testkit::terminate(pid)
            && !std::thread::panicking()
        {
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

/// The same three colours on every line, as fast as the shell can print.
fn flood_workload() -> String {
    "i=0; while :; do i=$((i+1)); printf '\\033[38;2;120;220;180mT\\033[38;5;208mI\\033[34mB\\033[0m flood %d\\n' \"$i\"; done"
        .to_string()
}

/// An attach client whose controlling terminal is a PTY the test owns.
struct AttachedClient {
    child: Child,
    master: File,
    /// Held so the line discipline outlives the child and can be inspected.
    _slave: OwnedFd,
    /// The PTY's modes before the child ran: what "restored" means.
    initial: Termios,
    seen: Arc<Mutex<Vec<u8>>>,
    reader: Option<Reader>,
}

/// Reads the PTY the moment the client writes to it, as a terminal does. A
/// signalled client gives its restore sequences a bounded time to drain, so a
/// test that read on a timer would race that bound.
struct Reader {
    stop: std::io::PipeWriter,
    thread: JoinHandle<()>,
}

/// One read of the non-blocking `master`. `false` once the PTY has nothing
/// more to give: empty for now, or closed.
fn read_once(mut master: &File, seen: &Mutex<Vec<u8>>) -> std::io::Result<bool> {
    let mut buf = [0u8; 8192];
    match master.read(&mut buf) {
        Ok(0) => Ok(false),
        Ok(n) => {
            lock(seen).extend_from_slice(&buf[..n]);
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => Ok(true),
        Err(e) => Err(e),
    }
}

/// Read everything `master` holds. Only for an exited client: a live one
/// could keep it from ever running dry.
fn drain(master: &File, seen: &Mutex<Vec<u8>>) {
    while let Ok(true) = read_once(master, seen) {}
}

fn lock(seen: &Mutex<Vec<u8>>) -> MutexGuard<'_, Vec<u8>> {
    seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Reader {
    fn spawn(master: &File, seen: Arc<Mutex<Vec<u8>>>) -> Self {
        let master = master.try_clone().expect("dup master");
        let (stopped, stop) = std::io::pipe().expect("stop pipe");
        let thread = std::thread::spawn(move || {
            loop {
                let mut fds = [
                    libc::pollfd {
                        fd: master.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                    libc::pollfd {
                        fd: stopped.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                ];
                // SAFETY: `fds` is a live array of two pollfds over
                // descriptors this closure owns.
                let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
                if ready < 0 {
                    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    return;
                }
                if fds[1].revents != 0 {
                    return;
                }
                let hung_up = fds[0].revents & (libc::POLLHUP | libc::POLLERR) != 0;
                match read_once(&master, &seen) {
                    Ok(true) => {}
                    Ok(false) if !hung_up => {}
                    _ => return,
                }
            }
        });
        Self { stop, thread }
    }

    fn stop(mut self) {
        let _ = self.stop.write_all(b"x");
        let _ = self.thread.join();
    }
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
        // Read before spawning: once the child runs it may be raw at any moment.
        let initial = tcgetattr(pty.slave.as_fd()).expect("tcgetattr slave");
        let child = cmd.spawn().expect("spawn attach");
        nix::fcntl::fcntl(
            &pty.master,
            nix::fcntl::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
        )
        .expect("set O_NONBLOCK");
        let master = File::from(pty.master);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let reader = Some(Reader::spawn(&master, seen.clone()));
        Self {
            child,
            master,
            _slave: pty.slave,
            initial,
            seen,
            reader,
        }
    }

    fn seen(&self) -> Vec<u8> {
        lock(&self.seen).clone()
    }

    /// Stop reading the PTY, as a terminal that hung would.
    fn stop_reading(&mut self) {
        if let Some(reader) = self.reader.take() {
            reader.stop();
        }
    }

    /// Read through the master: macOS revokes the slave descriptors once the
    /// session leader, the client, has exited.
    fn termios(&self) -> Termios {
        tcgetattr(self.master.as_fd()).expect("tcgetattr master")
    }

    /// Wait until the pane's colours have been drawn: the client is past the
    /// handshake, in raw mode, and inside its loop.
    fn wait_until_drawing(&mut self) {
        let drawn = poll_until(BUDGET, || {
            if drew_every_probe(&lock(&self.seen)) {
                return true;
            }
            if let Ok(Some(status)) = self.child.try_wait() {
                panic!(
                    "attach exited ({status}) before drawing: {:?}",
                    String::from_utf8_lossy(&self.seen())
                );
            }
            false
        });
        assert!(
            drawn,
            "attach never drew the pane: {:?}",
            String::from_utf8_lossy(&self.seen())
        );
    }

    /// The client's exit status once it has exited; `None` while it runs.
    fn exit_status(&mut self) -> Option<std::process::ExitStatus> {
        poll(BUDGET, || self.child.try_wait().ok().flatten())
    }

    /// Wait for exit while reading the PTY, then take whatever it still holds.
    fn wait_for_exit(&mut self) -> std::process::ExitStatus {
        let status = self.exit_status();
        self.stop_reading();
        drain(&self.master, &self.seen);
        status.unwrap_or_else(|| {
            let seen = self.seen();
            let tail = &seen[seen.len().saturating_sub(400)..];
            panic!(
                "attach client still running after {BUDGET:?}; last output: {:?}",
                String::from_utf8_lossy(tail)
            )
        })
    }
}

impl Drop for AttachedClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.stop_reading();
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Every colour probe drawn: truecolor, indexed, and the basic SGR 34, which
/// the compositor re-encodes as palette slot 4.
fn drew_every_probe(seen: &[u8]) -> bool {
    [&b"38;2;120;220;180"[..], b"38;5;208", b"38;5;4m"]
        .iter()
        .all(|probe| find(seen, probe))
}

/// Whether a thread of `pid` sits in a write to its stdout or stderr: the
/// terminal stopped reading and the PTY is full.
#[cfg(target_os = "linux")]
fn blocked_writing_to_terminal(pid: u32) -> bool {
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return false;
    };
    tasks.flatten().any(|task| {
        let syscall = match std::fs::read_to_string(task.path().join("syscall")) {
            Ok(syscall) => syscall,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                panic!("cannot see where pid {pid} is blocked (ptrace_scope?): {e}")
            }
            Err(_) => String::new(),
        };
        let mut fields = syscall.split_whitespace();
        let nr = fields.next().and_then(|n| n.parse::<i64>().ok());
        let fd = fields.next();
        matches!(nr, Some(n) if n == nix::libc::SYS_write || n == nix::libc::SYS_writev)
            && matches!(fd, Some("0x1" | "0x2"))
    })
}

fn assert_signal_restores_terminal(sig: Signal) {
    let h = Harness::new();
    let session = format!("sig-{}", sig.as_str().to_ascii_lowercase());
    h.create_session(&session);

    let mut client = AttachedClient::spawn(&h, &session);
    let before = client.initial.clone();
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
    let drawn = client.seen().len();

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

    let seen = client.seen();
    let tail = &seen[drawn..];
    for (what, seq) in [
        ("left the alternate screen", &b"\x1b[?1049l"[..]),
        ("popped the kitty keyboard flags", b"\x1b[<1u"),
        ("turned mouse tracking off", b"\x1b[?1000l"),
        ("turned bracketed paste off", b"\x1b[?2004l"),
        ("showed the cursor", b"\x1b[?25h"),
    ] {
        // The end only: CI logs cut a long line, and the end is what matters.
        assert!(
            find(tail, seq),
            "{sig}: never {what} after the signal; {} bytes after it, ending {:?}",
            tail.len(),
            String::from_utf8_lossy(&tail[tail.len().saturating_sub(600)..])
        );
    }

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
    let before = client.initial.clone();
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
    let seen = client.seen();
    let text = String::from_utf8_lossy(&seen);
    assert!(
        text.contains("[session 'sig-session-kill' ended]"),
        "the client never said why it left: {text:?}"
    );
}

/// Connect a headless client and read until the pane's colours arrive.
async fn headless_attach(
    h: &Harness,
    session: &str,
) -> Framed<UnixStream, tokio_util::codec::LengthDelimitedCodec> {
    let sock = h.runtime_dir().join("shux").join("attach.sock");
    let stream = UnixStream::connect(&sock).await.expect("connect attach");
    let mut framed = Framed::new(stream, shux_rpc::create_codec());
    let hello = AttachHello {
        protocol: ATTACH_PROTOCOL_VERSION,
        session_name: Some(session.to_string()),
        cols: 100,
        rows: 30,
        client_version: "attach-teardown-test".to_string(),
        graphics: false,
    };
    framed
        .send(Bytes::from(serde_json::to_vec(&hello).unwrap()))
        .await
        .expect("send hello");
    let ready = framed.next().await.expect("ready").expect("ready bytes");
    assert!(
        matches!(
            serde_json::from_slice::<AttachReady>(&ready).expect("parse ready"),
            AttachReady::Ok { .. }
        ),
        "attach denied"
    );
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + BUDGET;
    while !drew_every_probe(&seen) {
        let frame = tokio::time::timeout_at(deadline, framed.next())
            .await
            .expect("pane never drawn")
            .expect("daemon hung up before drawing the pane")
            .expect("read frame");
        if let Ok(AttachServerFrame::Render { data }) = serde_json::from_slice(&frame) {
            seen.extend(BASE64.decode(&data).expect("render base64"));
        }
    }
    framed
}

/// After the frame that ends an attach, the daemon sends nothing more and
/// hangs up, rather than rendering into a socket nobody is reading.
async fn assert_hung_up_after(
    framed: &mut Framed<UnixStream, tokio_util::codec::LengthDelimitedCodec>,
    is_last: fn(&AttachServerFrame) -> bool,
) {
    let deadline = tokio::time::Instant::now() + BUDGET;
    loop {
        let frame = tokio::time::timeout_at(deadline, framed.next())
            .await
            .expect("no final frame");
        let parsed: AttachServerFrame =
            serde_json::from_slice(&frame.expect("closed before the final frame").unwrap())
                .expect("parse frame");
        if is_last(&parsed) {
            break;
        }
    }
    let after = tokio::time::timeout(BUDGET, framed.next())
        .await
        .expect("daemon kept the socket open after the final frame");
    assert!(
        after.is_none(),
        "daemon sent a frame after the final one: {:?}",
        after.map(|r| r.map(|b| String::from_utf8_lossy(&b).into_owned()))
    );
}

#[tokio::test]
async fn a_detached_client_is_sent_nothing_more_and_hung_up_on() {
    let h = Harness::new();
    // A pane that never stops printing keeps the renderer queueing frames
    // right up to the detach; none may follow the DetachAck.
    h.create_session_running("teardown-detach", &flood_workload());
    let mut framed = headless_attach(&h, "teardown-detach").await;
    framed
        .send(Bytes::from(
            serde_json::to_vec(&AttachClientFrame::Detach).unwrap(),
        ))
        .await
        .expect("send detach");
    assert_hung_up_after(&mut framed, |f| matches!(f, AttachServerFrame::DetachAck)).await;
}

#[tokio::test]
async fn a_client_whose_session_ends_is_hung_up_on() {
    let h = Harness::new();
    h.create_session("teardown-ended");
    let mut framed = headless_attach(&h, "teardown-ended").await;
    h.kill_session("teardown-ended");
    assert_hung_up_after(&mut framed, |f| {
        matches!(f, AttachServerFrame::SessionEnded { .. })
    })
    .await;
}

/// A terminal that stops reading (a stalled ssh, a hung emulator) leaves the
/// client blocked mid-write. A signal must still end it and restore the modes:
/// that is exactly when someone reaches for `kill`. Linux only: proving the
/// client really is blocked reads `/proc`.
#[cfg(target_os = "linux")]
#[test]
fn a_signal_ends_a_client_whose_terminal_stopped_reading() {
    let h = Harness::new();
    h.create_session_running("sig-stalled", &flood_workload());
    let mut client = AttachedClient::spawn(&h, "sig-stalled");
    let before = client.initial.clone();
    client.wait_until_drawing();

    // Stop reading and let the client's output back up behind the PTY.
    client.stop_reading();
    assert!(
        poll_until(BUDGET, || blocked_writing_to_terminal(client.child.id())),
        "the client never blocked on its terminal; this test proves nothing"
    );
    kill(Pid::from_raw(client.child.id() as i32), Signal::SIGTERM).expect("signal attach");
    let status = client.exit_status();
    let after = client.termios();

    let status = status.expect("a client blocked on its terminal ignored SIGTERM");
    assert_eq!(
        after.local_flags, before.local_flags,
        "local flags not restored while the terminal was not reading"
    );
    assert_eq!(
        status.signal(),
        Some(Signal::SIGTERM as i32),
        "got {status}"
    );
}
