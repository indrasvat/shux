//! The production attach path, end to end, through a real daemon.
//!
//! Everything else about the emit path is unit-tested against a byte buffer.
//! That is exactly the shape that let a whole feature stay green while unwired
//! from the code a user runs, so this drives the real binary, a real PTY child
//! and the real attach socket, and reads what the daemon would have put on a
//! terminal.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use shux_rpc::attach::{ATTACH_PROTOCOL_VERSION, AttachHello, AttachReady, AttachServerFrame};
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

    fn rpc(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let out = Command::new(&self.bin)
            .env("XDG_RUNTIME_DIR", self.runtime_dir())
            .env("NO_COLOR", "1")
            .env("SHELL", "/bin/sh")
            .args([
                "--format",
                "json",
                "rpc",
                "call",
                method,
                "--params",
                &params.to_string(),
            ])
            .output()
            .unwrap_or_else(|e| panic!("shux rpc {method}: {e}"));
        assert!(
            out.status.success(),
            "shux rpc {method} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "shux rpc {method} returned non-JSON ({e}): {}",
                String::from_utf8_lossy(&out.stdout)
            )
        })
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let pid: Option<u32> = std::fs::read_to_string(self.runtime_dir().join("shux/shux.pid"))
            .ok()
            .and_then(|s| s.trim().parse().ok());
        // By pidfile, never by an argv substring: this process's own command
        // line matches any needle broad enough to find the daemon.
        if let Some(pid) = pid {
            let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
            for _ in 0..50 {
                if kill(Pid::from_raw(pid as i32), None).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            panic!("daemon {pid} survived SIGTERM");
        }
    }
}

/// A pane workload that draws a real image and the three colour classes, then
/// holds the pane open.
fn workload() -> String {
    // 18x38 px of solid red as raw RGB: exactly 2x2 cells at the declared
    // 9x19, and small enough to need no chunking.
    let px: Vec<u8> = std::iter::repeat_n([200u8, 40, 40], 18 * 38)
        .flatten()
        .collect();
    let b64 = base64::engine::general_purpose::STANDARD.encode(&px);
    format!(
        "printf '\\033_Ga=T,f=24,s=18,v=38,t=d;{b64}\\033\\\\\\\\'; \
         printf '\\033[38;2;120;220;180mTRUECOLOR\\033[0m \
\\033[38;5;208mINDEXED\\033[0m \\033[34mBASIC\\033[0m\\n'; \
         while :; do sleep 3600; done"
    )
}

/// Collect the bytes the daemon would have written to a terminal, for a client
/// that either can or cannot draw images.
async fn render_bytes(h: &Harness, session: &str, graphics: bool) -> Vec<u8> {
    let sock = h.runtime_dir().join("shux").join("attach.sock");
    let stream = UnixStream::connect(&sock).await.expect("connect attach");
    let mut framed = Framed::new(stream, shux_rpc::create_codec());
    let hello = AttachHello {
        protocol: ATTACH_PROTOCOL_VERSION,
        session_name: Some(session.to_string()),
        cols: 100,
        rows: 30,
        client_version: "image-retransmit-test".to_string(),
        graphics,
    };
    framed
        .send(Bytes::from(serde_json::to_vec(&hello).unwrap()))
        .await
        .expect("send hello");
    let ready = framed.next().await.expect("ready").expect("ready bytes");
    match serde_json::from_slice::<AttachReady>(&ready).expect("parse ready") {
        AttachReady::Ok { .. } => {}
        AttachReady::Error { code, message } => panic!("attach denied: {code}: {message}"),
    }

    // Read render frames until everything the caller asserts on has arrived, or
    // the budget runs out.
    // Bounded by a deadline rather than a frame count: the daemon coalesces,
    // so "how many frames" is not a stable quantity to wait on.
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while tokio::time::Instant::now() < deadline {
        let Ok(Some(Ok(frame))) = tokio::time::timeout(Duration::from_secs(2), framed.next()).await
        else {
            continue;
        };
        if let Ok(AttachServerFrame::Render { data }) = serde_json::from_slice(&frame)
            && let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(&data)
        {
            seen.extend_from_slice(&raw);
        }
        // Stop only when everything the assertions read is present. Breaking
        // on the image alone stopped between the workload's two printfs, and
        // failed on a colour probe the daemon had not written yet.
        let probes = find(&seen, b"38;2;120;220;180") && find(&seen, b"38;5;208");
        if probes && (!graphics || find(&seen, b"\x1b_Ga=T")) {
            break;
        }
    }
    seen
}

fn find(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// The control block of the first `a=T`, as text.
fn transmit_keys(bytes: &[u8]) -> String {
    let at = bytes
        .windows(6)
        .position(|w| w == b"\x1b_Ga=T")
        .expect("no transmit in the render stream");
    let rest = &bytes[at + 3..];
    let end = rest.iter().position(|&b| b == b';').expect("no payload");
    String::from_utf8_lossy(&rest[..end]).into_owned()
}

#[tokio::test]
async fn a_real_attach_carries_a_panes_picture_to_the_terminal() {
    let h = Harness::new();
    h.rpc(
        "session.create",
        serde_json::json!({ "name": "img-attach", "command": workload() }),
    );

    let bytes = render_bytes(&h, "img-attach", true).await;
    assert!(
        find(&bytes, b"\x1b_Ga=T"),
        "the daemon sent an attached client no picture at all"
    );

    let keys = transmit_keys(&bytes);
    assert!(keys.contains("C=1"), "{keys}");
    assert!(keys.contains("q=2"), "{keys}");
    assert!(keys.contains("s=18,v=38"), "{keys}");
    assert!(keys.contains("c=2,r=2"), "{keys}");

    // Colour probes: a monochrome regression must not be able to pass this.
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("38;2;120;220;180"), "truecolor probe missing");
    assert!(text.contains("38;5;208"), "indexed probe missing");
}

#[tokio::test]
async fn a_client_that_reports_no_graphics_is_sent_no_pictures() {
    // The wire field, end to end.
    let h = Harness::new();
    h.rpc(
        "session.create",
        serde_json::json!({ "name": "img-nographics", "command": workload() }),
    );

    let bytes = render_bytes(&h, "img-nographics", false).await;
    assert!(
        !find(&bytes, b"\x1b_G"),
        "sent graphics to a client whose terminal never claimed to draw them"
    );
    // Still a working attach: the compositor writes one CUP per cell, so the
    // probe words are never contiguous here -- the SGR introducers are what
    // survive, and what a monochrome regression would lose.
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("38;2;120;220;180"),
        "the pane's colour never arrived"
    );
    assert!(text.contains("38;5;208"), "indexed colour never arrived");
}

/// What the real binary writes to a terminal before the alternate screen takes
/// over. `session attach` refuses a redirected stdin/stdout, so a pipe cannot
/// reach this code at all -- the check has to own a pty.
fn attach_pty_output(h: &Harness, session: &str, graphics: &str) -> String {
    use std::io::Read;
    use std::os::fd::OwnedFd;
    use std::process::Stdio;

    let pty = nix::pty::openpty(None, None).expect("openpty");
    let slave: OwnedFd = pty.slave;
    let dup = |fd: &OwnedFd| Stdio::from(fd.try_clone().expect("dup slave"));
    let mut child = Command::new(&h.bin)
        .env("XDG_RUNTIME_DIR", h.runtime_dir())
        .env("NO_COLOR", "1")
        .env("TERM", "xterm-256color")
        .env("SHUX_GRAPHICS", graphics)
        .args(["session", "attach", session])
        .stdin(dup(&slave))
        .stdout(dup(&slave))
        .stderr(dup(&slave))
        .spawn()
        .expect("spawn attach");
    drop(slave);

    // Non-blocking, because the child stays alive holding the screen: the read
    // has to end on a deadline rather than on EOF.
    let master = pty.master;
    nix::fcntl::fcntl(&master, nix::fcntl::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK))
        .expect("set O_NONBLOCK");
    let mut file = std::fs::File::from(master);
    let mut seen = String::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        let mut buf = [0u8; 4096];
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => seen.push_str(&String::from_utf8_lossy(&buf[..n])),
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
        if seen.contains("SHUX_GRAPHICS") {
            break;
        }
    }
    // By recorded pid, and reaped, so the suite leaves nothing behind.
    let _ = child.kill();
    let _ = child.wait();
    seen
}

#[tokio::test]
async fn a_misspelled_graphics_override_says_so_where_a_person_can_see_it() {
    // The hatch fails OPEN, so a near-miss spelling silently becomes the
    // corruption it exists to prevent. The diagnostic must therefore reach the
    // user, not a log: without `-v` the client's subscriber is ERROR-only, so a
    // `warn!` here goes nowhere at all.
    let h = Harness::new();
    h.rpc(
        "session.create",
        serde_json::json!({ "name": "gfx-typo", "command": workload() }),
    );

    let seen = attach_pty_output(&h, "gfx-typo", "Onn");
    assert!(
        seen.contains("ignoring SHUX_GRAPHICS"),
        "a near-miss override was accepted in silence; the terminal saw: {seen:?}"
    );

    // And a spelling that IS recognised says nothing.
    let quiet = attach_pty_output(&h, "gfx-typo", "off");
    assert!(
        !quiet.contains("ignoring SHUX_GRAPHICS"),
        "a valid override warned anyway: {quiet:?}"
    );
}
