//! What a real `shux session attach` client delivers from its terminal to the
//! pane's program (issue #197): a large paste whole and a bracketed paste,
//! with nothing burned while idle.
//!
//! Everything is real: the daemon, the attach client on a PTY of its own, and a
//! program in the pane that reads what arrives and writes it to a file. The
//! client turns a paste into one input frame per key, far more frames than a
//! pane's channel holds.

use std::io::{Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::time::Instant;

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use shux_testkit::BUDGET;
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
        if let Some(pid) = pid
            && !shux_testkit::terminate(pid)
            && !std::thread::panicking()
        {
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

/// Whether the pane's program wrote `path` within [`BUDGET`], for a caller
/// that reports what did arrive before it fails.
fn appears(path: &Path) -> bool {
    shux_testkit::poll_until(BUDGET, || path.exists())
}

/// Each word is split in the command, so only the program's output holds it.
const PROBES: &str = "printf '\\033[38;2;120;220;180mTRUE%sCOLOR\\033[0m \\033[38;5;208mINDE%sXED\\033[0m \\033[34mBA%sSIC\\033[0m\\r\\n' '' '' ''";

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
    shux_testkit::wait_until("the pane's program to reach raw mode", || ready.exists());
}

/// A real attach client on a PTY of its own.
struct Client {
    keys: std::fs::File,
    screen: Arc<std::sync::Mutex<Vec<u8>>>,
    /// Closing it wakes the drain and ends it.
    stop: Option<OwnedFd>,
    drain: Option<std::thread::JoinHandle<()>>,
    process: KillOnDrop,
}

impl Client {
    fn attach(h: &Harness) -> Self {
        let mut command = Command::new(&h.bin);
        command.args(["session", "attach", "paste"]);
        Self::run(h, command)
    }

    /// Attaches with the terminal as the client's controlling terminal and
    /// its stdin a pipe already at end of file, as `true | nohup shux session
    /// create` leaves it: the client reads the terminal through `/dev/tty`,
    /// and no SIGHUP reaches it.
    #[cfg(target_os = "linux")]
    fn attach_without_stdin(h: &Harness) -> Self {
        use std::os::fd::AsRawFd;
        let (read, write) = nix::unistd::pipe().expect("pipe");
        drop(write);
        // `setsid -c` takes the terminal from stdin, so the pipe replaces it
        // only afterwards, in the shell that then becomes the client.
        let fd = read.as_raw_fd();
        let mut command = Command::new("setsid");
        command.args([
            "-c",
            "sh",
            "-c",
            &format!(r#"trap '' HUP; exec "$0" session create paste --ensure 0<&{fd} {fd}<&-"#),
        ]);
        command.arg(&h.bin);
        let client = Self::run(h, command);
        drop(read);
        client
    }

    fn run(h: &Harness, mut command: Command) -> Self {
        let size = nix::pty::Winsize {
            ws_row: 30,
            ws_col: 100,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = nix::pty::openpty(Some(&size), None).expect("openpty");
        // Not inherited: a client holding the terminal's other end open would
        // keep it from ever hanging up.
        for fd in [&pty.master, &pty.slave] {
            nix::fcntl::fcntl(fd, nix::fcntl::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC))
                .expect("set FD_CLOEXEC");
        }
        let slave: OwnedFd = pty.slave;
        let dup = |fd: &OwnedFd| Stdio::from(fd.try_clone().expect("dup slave"));
        let process = command
            .env("XDG_RUNTIME_DIR", h.runtime_dir())
            .env_remove("SHUX_SOCKET")
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
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
        let (stopped, stop) = nix::unistd::pipe().expect("pipe");
        for fd in [&stopped, &stop] {
            nix::fcntl::fcntl(fd, nix::fcntl::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC))
                .expect("set FD_CLOEXEC");
        }
        let screen = Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let drain = {
            let screen = screen.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 16 * 1024];
                loop {
                    let mut fds = [
                        PollFd::new(screen_side.as_fd(), PollFlags::POLLIN),
                        PollFd::new(stopped.as_fd(), PollFlags::POLLIN),
                    ];
                    match poll(&mut fds, PollTimeout::NONE) {
                        Ok(_) => {}
                        Err(Errno::EINTR) => continue,
                        Err(_) => break,
                    }
                    if fds[1].revents().is_some_and(|r| !r.is_empty()) {
                        break;
                    }
                    match screen_side.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => screen
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .extend_from_slice(&buf[..n]),
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                            ) => {}
                        // EIO: the client has closed its terminal.
                        Err(_) => break,
                    }
                }
            })
        };
        let client = Self {
            keys,
            screen,
            stop: Some(stop),
            drain: Some(drain),
            process,
        };

        // The client draws its first frame only once raw mode is on and the
        // attach is live; keys typed before that are cooked by its terminal.
        client.wait_for_text("TRUECOLOR");
        client.assert_colour_probes();
        client
    }

    fn wait_for_text(&self, text: &str) {
        shux_testkit::wait_until(&format!("the attach client to draw {text:?}"), || {
            find_cell(&self.screen(), text).is_some()
        });
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

    /// Pastes `text` as a terminal does: between the bracketed-paste markers
    /// only while the client has mode 2004 turned on.
    fn paste(&mut self, text: &[u8]) {
        let asked = {
            let seen = self.screen.lock().unwrap_or_else(|e| e.into_inner());
            let last = |seq: &[u8]| seen.windows(seq.len()).rposition(|w| w == seq);
            last(b"\x1b[?2004h") > last(b"\x1b[?2004l")
        };
        assert!(
            asked,
            "the attach client never asked its terminal for bracketed paste"
        );
        self.type_bytes(&[b"\x1b[200~", text, b"\x1b[201~"].concat());
    }

    /// The master shares the drain's non-blocking file description, so a full
    /// terminal input queue answers WouldBlock: that is the client applying
    /// backpressure, and the typist waits for room like a real one would.
    ///
    /// Room is found by trying the write: POLLOUT on a pty master wakes only
    /// when the reader drains it below the tty's low-water mark, so a client
    /// reading a little at a time would never wake a waiter.
    fn type_bytes(&mut self, bytes: &[u8]) {
        let mut rest = bytes;
        while !rest.is_empty() {
            let wrote = shux_testkit::poll(BUDGET, || match self.keys.write(rest) {
                Ok(n) => Some(n),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    None
                }
                Err(e) => panic!("type into the client: {e}"),
            });
            let Some(n) = wrote else {
                panic!(
                    "the attach client read no key for {BUDGET:?} with {} left to type",
                    rest.len()
                );
            };
            rest = &rest[n..];
        }
    }

    fn stop_drain(&mut self) {
        drop(self.stop.take());
        if let Some(drain) = self.drain.take() {
            let _ = drain.join();
        }
    }

    /// Closes the terminal, as closing its window does, and waits up to
    /// [`BUDGET`] for the client to exit.
    fn hang_up(&mut self) -> bool {
        self.stop_drain();
        self.keys = std::fs::File::open("/dev/null").expect("open /dev/null");
        shux_testkit::poll_until(BUDGET, || matches!(self.process.0.try_wait(), Ok(Some(_))))
    }

    #[cfg(target_os = "linux")]
    fn pid(&self) -> u32 {
        self.process.0.id()
    }

    /// The thread that reads the client's terminal. tokio names its workers,
    /// so it is the one thread besides the main one that keeps the process's
    /// own name.
    #[cfg(target_os = "linux")]
    fn input_thread(&self) -> u32 {
        let pid = self.pid();
        let comm = |tid: u32| std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/comm")).ok();
        shux_testkit::wait_for("one unnamed thread in the attach client", || {
            let own = comm(pid)?;
            let mut unnamed = std::fs::read_dir(format!("/proc/{pid}/task"))
                .ok()?
                .filter_map(|t| t.ok()?.file_name().to_str()?.parse::<u32>().ok())
                .filter(|&tid| tid != pid && comm(tid).as_ref() == Some(&own));
            let tid = unnamed.next()?;
            unnamed.next().is_none().then_some(tid)
        })
    }

    /// How many times thread `tid` has blocked; `None` once it has exited.
    #[cfg(target_os = "linux")]
    fn blocks(&self, tid: u32) -> Option<u64> {
        let status =
            std::fs::read_to_string(format!("/proc/{}/task/{tid}/status", self.pid())).ok()?;
        status
            .lines()
            .find_map(|l| l.strip_prefix("voluntary_ctxt_switches:"))?
            .trim()
            .parse()
            .ok()
    }

    /// Waits for the input thread `tid` to come back from `n` poll timeouts
    /// with nothing typed. Each blocks it again, and before it does it has run
    /// the checks the client makes when idle. False if the thread exited.
    #[cfg(target_os = "linux")]
    fn idle_polls(&self, tid: u32, n: u64) -> bool {
        let Some(start) = self.blocks(tid) else {
            return false;
        };
        // One more than `n`: `start` may have been read before it first blocked.
        let past = || match self.blocks(tid) {
            None => Some(false),
            Some(blocks) => (blocks > start + n).then_some(true),
        };
        shux_testkit::wait_for("the client's input thread to time out idle", past)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.stop_drain();
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
    let arrived = appears(&done);
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
    client.paste(b"hello world");
    let arrived = appears(&done);
    let received = std::fs::read(&got).unwrap_or_default();
    drop(client);
    assert!(
        arrived,
        "the pane's program got {:?} of a bracketed paste",
        String::from_utf8_lossy(&received)
    );
    assert_eq!(received, b"hello world");
}

/// A pane program that asked for bracketed paste (mode 2004, as bash's
/// readline and vim do) gets the paste between the markers: without them a
/// pasted newline runs the line instead of landing in the edit buffer.
#[test]
fn a_paste_into_a_program_that_asked_for_brackets_keeps_them() {
    let h = Harness::new();
    let (got, done) = (h.work.path().join("got"), h.work.path().join("done"));
    let want: &[u8] = b"\x1b[200~echo a\recho b\r\x1b[201~";
    pane_session(
        &h,
        &format!(
            "printf '\\033[?2004hAR%sMED\\r\\n' ''; head -c {n} > '{got}'; : > '{done}'; \
             while :; do sleep 3600; done",
            n = want.len(),
            got = got.display(),
            done = done.display(),
        ),
    );
    let mut client = Client::attach(&h);
    // Drawn after the mode switch, so the daemon has seen it.
    client.wait_for_text("ARMED");
    client.paste(b"echo a\recho b\r");
    let arrived = appears(&done);
    let received = std::fs::read(&got).unwrap_or_default();
    drop(client);
    assert!(
        arrived,
        "the pane's program got {:?} of a bracketed paste",
        String::from_utf8_lossy(&received)
    );
    assert_eq!(
        String::from_utf8_lossy(&received),
        String::from_utf8_lossy(want)
    );
}

/// A paste far larger than one frame still reaches the program as one
/// paste: one start marker, one end marker, and the text whole between them.
#[test]
fn a_paste_larger_than_a_frame_is_still_one_paste() {
    const LEN: usize = 5 << 20;
    let h = Harness::new();
    let (got, done) = (h.work.path().join("got"), h.work.path().join("done"));
    pane_session(
        &h,
        &format!(
            "printf '\\033[?2004hAR%sMED\\r\\n' ''; head -c {n} > '{got}'; : > '{done}'; \
             while :; do sleep 3600; done",
            n = LEN + 12,
            got = got.display(),
            done = done.display(),
        ),
    );
    let mut client = Client::attach(&h);
    client.wait_for_text("ARMED");
    let text: Vec<u8> = paste_bytes().iter().copied().cycle().take(LEN).collect();
    client.paste(&text);
    let arrived = appears(&done);
    let received = std::fs::read(&got).unwrap_or_default();
    drop(client);
    assert!(arrived, "{} of {} bytes arrived", received.len(), LEN + 12);
    let count = |marker: &[u8]| {
        received
            .windows(marker.len())
            .filter(|w| *w == marker)
            .count()
    };
    assert_eq!(
        (count(b"\x1b[200~"), count(b"\x1b[201~")),
        (1, 1),
        "the paste reached the program as more than one"
    );
    assert!(
        received[6..received.len() - 6] == text[..],
        "the paste's text changed"
    );
}

/// A terminal can go away without a SIGHUP reaching the client: under
/// `nohup`, or when it is not the client's controlling terminal, as here.
/// The client must notice and exit rather than poll a dead terminal forever.
#[test]
fn a_client_whose_terminal_hung_up_exits() {
    let h = Harness::new();
    pane_session(&h, "while :; do sleep 3600; done");
    let mut client = Client::attach(&h);
    assert!(
        client.hang_up(),
        "the client still runs {BUDGET:?} after its terminal hung up"
    );
}

/// A client reading its terminal through `/dev/tty` has nothing to watch on
/// stdin: an empty pipe there is no hangup, and the attach stays up.
#[cfg(target_os = "linux")]
#[test]
fn a_client_without_stdin_stays_attached() {
    let h = Harness::new();
    pane_session(
        &h,
        "head -c 3 > /dev/null; printf 'GOT-%sKEYS\\n' ''; sleep 3600",
    );
    let mut client = Client::attach_without_stdin(&h);
    // The client looks for a hangup each time its wait for a key times out.
    let input = client.input_thread();
    let idled = client.idle_polls(input, 2);
    assert!(
        idled && matches!(client.process.0.try_wait(), Ok(None)),
        "the client left an idle attach because its stdin was an empty pipe"
    );
    client.type_bytes(b"abc");
    client.wait_for_text("GOT-KEYS");
}

/// The terminal a client reads through `/dev/tty` is the one it watches for
/// a hangup, whatever its stdin is.
#[cfg(target_os = "linux")]
#[test]
fn a_client_reading_dev_tty_exits_when_it_hangs_up() {
    let h = Harness::new();
    pane_session(&h, "while :; do sleep 3600; done");
    let mut client = Client::attach_without_stdin(&h);
    assert!(
        client.hang_up(),
        "the client still runs {BUDGET:?} after its terminal hung up"
    );
}

/// An attach client with nothing to do must not spin: a multiplexer sits
/// attached and idle for hours.
#[cfg(target_os = "linux")]
#[test]
fn an_idle_attach_client_sleeps() {
    let h = Harness::new();
    pane_session(&h, "while :; do sleep 3600; done");
    let client = Client::attach(&h);
    let input = client.input_thread();
    // Past the first frame's work: the client has sat through an idle poll.
    assert!(
        client.idle_polls(input, 1),
        "the client's input thread exited"
    );

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
    // The window is the client's own idle cycle, however long it takes to run.
    let (start, started) = (cpu(client.pid()), Instant::now());
    assert!(
        client.idle_polls(input, 4),
        "the client's input thread exited"
    );
    let (end, window) = (cpu(client.pid()), started.elapsed());
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
