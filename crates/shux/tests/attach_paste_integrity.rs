//! Issue #197: a large paste typed through a real `shux session attach` client
//! reaches the pane's program whole.
//!
//! Everything is real: the daemon, the attach client on a PTY of its own, and a
//! program in the pane that reads the paste and writes what it got to a file.
//! The client turns a paste into one input frame per key, far more frames than
//! a pane's channel holds, which is what this has to survive.

use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use shux_vt::{Color, VirtualTerminal};

const PASTE_LEN: usize = 128 * 1024;

struct Harness {
    bin: PathBuf,
    runtime: tempfile::TempDir,
    work: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        Self {
            bin: PathBuf::from(env!("CARGO_BIN_EXE_shux")),
            runtime: tempfile::tempdir().expect("temp runtime dir"),
            work: tempfile::tempdir().expect("temp work dir"),
        }
    }

    fn runtime_dir(&self) -> &Path {
        self.runtime.path()
    }

    fn rpc(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let out = Command::new(&self.bin)
            .env("XDG_RUNTIME_DIR", self.runtime_dir())
            .env_remove("SHUX_SOCKET")
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
        serde_json::from_slice(&out.stdout).expect("rpc json")
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

/// Reaped on every exit path, a failed assert included.
struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Printable, so the pane's echo stays plain text, and aperiodic over any
/// window a dropped run could hide in.
fn paste_bytes() -> Vec<u8> {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut x: u32 = 0x9E37_79B9;
    (0..PASTE_LEN)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            ALPHABET[(x as usize) % ALPHABET.len()]
        })
        .collect()
}

fn wait_for(path: &Path, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn a_large_paste_through_a_real_attach_client_arrives_whole() {
    let h = Harness::new();
    let dir = h.work.path();
    let (ready, got, done) = (dir.join("ready"), dir.join("got"), dir.join("done"));

    // Raw mode first, so the paste is neither line-edited nor capped at the
    // canonical line limit; echo stays on, so the pane redraws as a real one
    // does while it is pasted into. `ready` is written only once stty returned.
    // The colour probes prove the pane renders colour classes intact.
    let reader = format!(
        "printf '\\033[38;2;120;220;180mTRUECOLOR\\033[0m \\033[38;5;208mINDEXED\\033[0m \
         \\033[34mBASIC\\033[0m\\r\\n'; stty raw; : > '{ready}'; \
         head -c {PASTE_LEN} > '{got}'; : > '{done}'; while :; do sleep 3600; done",
        ready = ready.display(),
        got = got.display(),
        done = done.display(),
    );
    h.rpc(
        "session.create",
        serde_json::json!({ "name": "paste", "command": reader }),
    );
    assert!(
        wait_for(&ready, Duration::from_secs(20)),
        "the pane's reader never reached raw mode"
    );

    let size = nix::pty::Winsize {
        ws_row: 30,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let pty = nix::pty::openpty(Some(&size), None).expect("openpty");
    let slave: OwnedFd = pty.slave;
    let dup = |fd: &OwnedFd| Stdio::from(fd.try_clone().expect("dup slave"));
    let client = Command::new(&h.bin)
        .env("XDG_RUNTIME_DIR", h.runtime_dir())
        .env_remove("SHUX_SOCKET")
        .env("TERM", "xterm-256color")
        .env("COLORTERM", "truecolor")
        .args(["session", "attach", "paste"])
        .stdin(dup(&slave))
        .stdout(dup(&slave))
        .stderr(dup(&slave))
        .spawn()
        .expect("spawn attach client");
    let client = KillOnDrop(client);
    drop(slave);

    // Drain the client's screen for as long as it runs: a terminal nobody
    // reads stops the client writing, and with it reading keys.
    let master = std::fs::File::from(pty.master);
    let mut screen_side = master.try_clone().expect("clone master");
    let stop = Arc::new(AtomicBool::new(false));
    let screen = Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let drain = {
        let (stop, screen) = (stop.clone(), screen.clone());
        nix::fcntl::fcntl(
            &screen_side,
            nix::fcntl::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
        )
        .expect("set O_NONBLOCK");
        std::thread::spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            while !stop.load(Ordering::Relaxed) {
                match screen_side.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => screen
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .extend_from_slice(&buf[..n]),
                    Err(_) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        })
    };

    // The client draws its first frame only once raw mode is on and the
    // attach is live; keys typed before that are cooked by its terminal.
    let deadline = Instant::now() + Duration::from_secs(20);
    while find_cell(&screen_of(&screen), "TRUECOLOR").is_none() {
        assert!(
            Instant::now() < deadline,
            "the attach client never drew the pane"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    // Before the paste scrolls them away: the pane's colour classes reach
    // the client's terminal intact.
    let vt = screen_of(&screen);
    for (probe, want) in [
        ("TRUECOLOR", Color::Rgb(120, 220, 180)),
        ("INDEXED", Color::Indexed(208)),
        ("BASIC", Color::Indexed(4)),
    ] {
        let (row, col) = find_cell(&vt, probe).unwrap_or_else(|| panic!("{probe} never drew"));
        let fg = vt.grid().visible_row(row)[col].style.fg;
        assert_eq!(fg, want, "{probe} drew in the wrong colour");
    }

    let paste = paste_bytes();
    // The master shares the drain's non-blocking file description, so a full
    // terminal input queue answers WouldBlock: that is the client applying
    // backpressure, and the typist waits for it like a real one would.
    let mut keys = master;
    let mut rest = &paste[..];
    let typing_deadline = Instant::now() + Duration::from_secs(60);
    while !rest.is_empty() {
        match keys.write(rest) {
            Ok(n) => rest = &rest[n..],
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < typing_deadline,
                    "the attach client stopped reading keys with {} left to type",
                    rest.len()
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(e) => panic!("type the paste: {e}"),
        }
    }

    // Nothing is typed after the paste: under an event source that stops
    // reading before the terminal is drained, the tail waits for a next key
    // that never comes.
    let arrived = wait_for(&done, Duration::from_secs(60));
    let received = std::fs::read(&got).unwrap_or_default();

    stop.store(true, Ordering::Relaxed);
    drop(client);
    let _ = drain.join();

    assert!(
        arrived,
        "{} of {PASTE_LEN} pasted bytes reached the pane's program",
        received.len()
    );
    assert_eq!(received.len(), PASTE_LEN, "short paste");
    if let Some(at) = received.iter().zip(&paste).position(|(a, b)| a != b) {
        panic!("the paste arrived whole in length but differs from byte {at}");
    }
}

/// What the client's terminal shows: its output replayed through a VT the
/// size of the PTY. The renderer positions every cell, so the raw bytes never
/// hold a word contiguously.
fn screen_of(bytes: &std::sync::Mutex<Vec<u8>>) -> VirtualTerminal {
    let mut vt = VirtualTerminal::new(30, 100);
    vt.process(&bytes.lock().unwrap_or_else(|e| e.into_inner()));
    vt
}

fn find_cell(vt: &VirtualTerminal, word: &str) -> Option<(usize, usize)> {
    let grid = vt.grid();
    (0..grid.rows()).find_map(|r| {
        let row = grid.visible_row(r);
        let text: String = (0..row.len()).map(|c| row[c].ch).collect();
        text.find(word)
            .map(|byte| (r, text[..byte].chars().count()))
    })
}
