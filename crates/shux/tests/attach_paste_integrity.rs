//! What a real `shux session attach` client delivers from its terminal to the
//! pane's program (issue #197): a large paste whole and a bracketed paste,
//! with nothing burned while idle.
//!
//! Everything is real: the daemon, the attach client on a PTY of its own, and a
//! program in the pane that reads what arrives and writes it to a file. The
//! client turns a paste into one input frame per key, far more frames than a
//! pane's channel holds.

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

const PROBES: &str = "printf '\\033[38;2;120;220;180mTRUECOLOR\\033[0m \\033[38;5;208mINDEXED\\033[0m \\033[34mBASIC\\033[0m\\r\\n'";

/// A session whose pane prints the colour probes, enters raw mode, touches
/// `ready` and then runs `then`.
fn pane_session(h: &Harness, then: &str) {
    let ready = h.work.path().join("ready");
    // Raw mode first, so input is neither line-edited nor capped at the
    // canonical line limit; echo stays on, so the pane redraws as a real one
    // does while it is typed into. `ready` is written only once stty returned.
    let command = format!(
        "{PROBES}; stty raw; : > '{ready}'; {then}",
        ready = ready.display()
    );
    h.rpc(
        "session.create",
        serde_json::json!({ "name": "paste", "command": command }),
    );
    assert!(
        wait_for(&ready, Duration::from_secs(20)),
        "the pane's program never reached raw mode"
    );
}

/// A real attach client on a PTY of its own.
struct Client {
    keys: std::fs::File,
    screen: Arc<std::sync::Mutex<Vec<u8>>>,
    stop: Arc<AtomicBool>,
    drain: Option<std::thread::JoinHandle<()>>,
    process: KillOnDrop,
}

impl Client {
    fn attach(h: &Harness) -> Self {
        let size = nix::pty::Winsize {
            ws_row: 30,
            ws_col: 100,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = nix::pty::openpty(Some(&size), None).expect("openpty");
        let slave: OwnedFd = pty.slave;
        let dup = |fd: &OwnedFd| Stdio::from(fd.try_clone().expect("dup slave"));
        let process = Command::new(&h.bin)
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
        let process = KillOnDrop(process);
        drop(slave);

        // Drain the client's screen for as long as it runs: a terminal nobody
        // reads stops the client writing, and with it reading keys.
        let keys = std::fs::File::from(pty.master);
        let mut screen_side = keys.try_clone().expect("clone master");
        nix::fcntl::fcntl(
            &screen_side,
            nix::fcntl::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
        )
        .expect("set O_NONBLOCK");
        let stop = Arc::new(AtomicBool::new(false));
        let screen = Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let drain = {
            let (stop, screen) = (stop.clone(), screen.clone());
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
        let client = Self {
            keys,
            screen,
            stop,
            drain: Some(drain),
            process,
        };

        // The client draws its first frame only once raw mode is on and the
        // attach is live; keys typed before that are cooked by its terminal.
        let deadline = Instant::now() + Duration::from_secs(20);
        while find_cell(&client.screen(), "TRUECOLOR").is_none() {
            assert!(
                Instant::now() < deadline,
                "the attach client never drew the pane"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        client.assert_colour_probes();
        client
    }

    fn screen(&self) -> VirtualTerminal {
        screen_of(&self.screen)
    }

    /// Before input scrolls them away: the pane's colour classes reach the
    /// client's terminal intact.
    fn assert_colour_probes(&self) {
        let vt = self.screen();
        for (probe, want) in [
            ("TRUECOLOR", Color::Rgb(120, 220, 180)),
            ("INDEXED", Color::Indexed(208)),
            ("BASIC", Color::Indexed(4)),
        ] {
            let (row, col) = find_cell(&vt, probe).unwrap_or_else(|| panic!("{probe} never drew"));
            let fg = vt.grid().visible_row(row)[col].style.fg;
            assert_eq!(fg, want, "{probe} drew in the wrong colour");
        }
    }

    /// The master shares the drain's non-blocking file description, so a full
    /// terminal input queue answers WouldBlock: that is the client applying
    /// backpressure, and the typist waits for it like a real one would.
    fn type_bytes(&mut self, bytes: &[u8]) {
        let mut rest = bytes;
        let deadline = Instant::now() + Duration::from_secs(60);
        while !rest.is_empty() {
            match self.keys.write(rest) {
                Ok(n) => rest = &rest[n..],
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "the attach client stopped reading keys with {} left to type",
                        rest.len()
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(e) => panic!("type into the client: {e}"),
            }
        }
    }

    fn pid(&self) -> u32 {
        self.process.0.id()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(drain) = self.drain.take() {
            let _ = drain.join();
        }
    }
}

#[test]
fn a_large_paste_through_a_real_attach_client_arrives_whole() {
    let h = Harness::new();
    let (got, done) = (h.work.path().join("got"), h.work.path().join("done"));
    pane_session(
        &h,
        &format!(
            "head -c {PASTE_LEN} > '{got}'; : > '{done}'; while :; do sleep 3600; done",
            got = got.display(),
            done = done.display(),
        ),
    );
    let mut client = Client::attach(&h);

    let paste = paste_bytes();
    client.type_bytes(&paste);

    // Nothing is typed after the paste: under an event source that stops
    // reading before the terminal is drained, the tail waits for a next key
    // that never comes.
    let arrived = wait_for(&done, Duration::from_secs(60));
    let received = std::fs::read(&got).unwrap_or_default();
    drop(client);

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

/// A terminal left in bracketed-paste mode by an earlier program wraps every
/// paste in `ESC[200~ … ESC[201~`. The paste is the user's text all the same.
#[test]
fn a_bracketed_paste_reaches_the_pane() {
    let h = Harness::new();
    let (got, done) = (h.work.path().join("got"), h.work.path().join("done"));
    pane_session(
        &h,
        &format!(
            "head -c 11 > '{got}'; : > '{done}'; while :; do sleep 3600; done",
            got = got.display(),
            done = done.display(),
        ),
    );
    let mut client = Client::attach(&h);
    client.type_bytes(b"\x1b[200~hello world\x1b[201~");
    let arrived = wait_for(&done, Duration::from_secs(10));
    let received = std::fs::read(&got).unwrap_or_default();
    drop(client);
    assert!(
        arrived,
        "the pane's program got {:?} of a bracketed paste",
        String::from_utf8_lossy(&received)
    );
    assert_eq!(received, b"hello world");
}

/// An attach client with nothing to do must not spin: a multiplexer sits
/// attached and idle for hours.
#[cfg(target_os = "linux")]
#[test]
fn an_idle_attach_client_sleeps() {
    let h = Harness::new();
    pane_session(&h, "while :; do sleep 3600; done");
    let client = Client::attach(&h);
    std::thread::sleep(Duration::from_secs(1));

    // CPU time each of the client's threads has run, in nanoseconds.
    let cpu = |pid: u32| -> std::collections::HashMap<String, u64> {
        std::fs::read_dir(format!("/proc/{pid}/task"))
            .expect("client tasks")
            .filter_map(|t| {
                let t = t.ok()?;
                let stat = std::fs::read_to_string(t.path().join("schedstat")).ok()?;
                let ns = stat.split_whitespace().next()?.parse().ok()?;
                Some((t.file_name().to_string_lossy().into_owned(), ns))
            })
            .collect()
    };
    let window = Duration::from_secs(5);
    let start = cpu(client.pid());
    std::thread::sleep(window);
    let end = cpu(client.pid());
    drop(client);

    let busiest_ms_per_s = end
        .iter()
        .map(|(tid, ns)| ns.saturating_sub(start.get(tid).copied().unwrap_or(0)))
        .max()
        .unwrap_or(0) as f64
        / 1e6
        / window.as_secs_f64();
    assert!(
        busiest_ms_per_s < 5.0,
        "an idle attach client's busiest thread ran {busiest_ms_per_s:.1} ms per second"
    );
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
