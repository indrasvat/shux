//! `shux events watch` and `shux pane watch` are streams meant for a pipe:
//! each item reaches the reader as it happens, and the command ends cleanly
//! when the reader closes the pipe.
//!
//! These drive the REAL `shux` binary against a REAL daemon, with stdout a
//! real pipe — the case a terminal-attached run never exercises. Each watch
//! runs with a long poll at the CLI's 30 s maximum, and every item must reach
//! the reader before that first poll could have ended: an item held until a
//! poll's deadline arrives after it.

use std::io::Read;
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

mod lens_common;
use lens_common::*;

const LONG_POLL: Duration = Duration::from_secs(30);
const LONG_POLL_MS: &str = "30000";

/// Kills the watcher if an assertion fails first, so no client outlives the test.
struct Watcher {
    child: Child,
    /// No poll the watcher makes can have started before this.
    spawned: Instant,
    reader: Option<JoinHandle<()>>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Watcher {
    fn spawn(h: &Harness, args: &[&str]) -> Self {
        let spawned = Instant::now();
        let child = h
            .shux()
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("spawn shux {args:?}: {e}"));
        Self {
            child,
            spawned,
            reader: None,
        }
    }

    /// The latest an item can arrive and still have beaten the long poll.
    fn before_the_long_poll_ends(&self) -> Instant {
        self.spawned + LONG_POLL
    }

    /// The watcher's stdout as chunks read on another thread, so a test waits
    /// on it with a timeout instead of blocking in `read`. Dropping the
    /// receiver closes the pipe at the next chunk.
    fn output(&mut self) -> mpsc::Receiver<Vec<u8>> {
        let mut stdout = self.child.stdout.take().expect("stdout");
        let (tx, rx) = mpsc::channel();
        self.reader = Some(std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = stdout.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        }));
        rx
    }

    /// Wait for the watcher to exit on its own once its reader has gone.
    /// `poke` makes it write: the first write lets the reader notice its
    /// receiver is gone and close the pipe, the next lands in the closed pipe.
    /// Returns status and stderr.
    fn exit_after_reader_closed(mut self, poke: impl Fn()) -> (std::process::ExitStatus, String) {
        let reader = self.reader.take().expect("output() was not taken");
        poke();
        assert!(
            shux_testkit::poll_until(BUDGET, || reader.is_finished()),
            "the reader never saw the watcher write after its receiver was dropped"
        );
        poke();
        let status = shux_testkit::poll(BUDGET, || self.child.try_wait().expect("try_wait"))
            .expect("the watcher was still running after its reader closed the pipe");
        let mut stderr = String::new();
        if let Some(mut e) = self.child.stderr.take() {
            let _ = e.read_to_string(&mut stderr);
        }
        (status, stderr)
    }
}

/// Collect output until it holds `needle`, failing at `by` even while output
/// keeps arriving.
fn read_until(rx: &mpsc::Receiver<Vec<u8>>, needle: &str, what: &str, by: Instant) -> String {
    let mut seen = String::new();
    while !seen.contains(needle) {
        let left = by.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(chunk) if !left.is_zero() => seen.push_str(&String::from_utf8_lossy(&chunk)),
            _ => {
                let tail = seen.len().saturating_sub(400);
                let tail = seen.ceil_char_boundary(tail);
                panic!(
                    "{what}: no {needle:?} before the watcher's {LONG_POLL:?} long poll could \
                     have ended; last of {} bytes seen: {:?}",
                    seen.len(),
                    &seen[tail..]
                )
            }
        }
    }
    seen
}

/// The seq the next published event will carry.
fn next_seq(h: &Harness) -> u64 {
    h.rpc_ok("events.history", serde_json::json!({"count": 1}))["current_seq"]
        .as_u64()
        .expect("current_seq")
}

fn create_session(h: &Harness, name: &str) {
    let out = h.cli(&["session", "create", name, "-d", "--", "sleep", "300"]);
    assert!(
        out.status.success(),
        "session create {name}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn pane_id_of(h: &Harness, session: &str) -> String {
    let sessions = h.rpc_ok("session.list", serde_json::json!({}));
    sessions["sessions"]
        .as_array()
        .and_then(|a| a.iter().find(|s| s["name"] == session))
        .and_then(|s| s["pane_id"].as_str())
        .unwrap_or_else(|| panic!("no {session} pane in {sessions}"))
        .to_string()
}

fn pane_watch(h: &Harness, session: &str, pane: &str) -> Watcher {
    Watcher::spawn(
        h,
        &[
            "pane",
            "watch",
            "-s",
            session,
            "-p",
            pane,
            "--timeout-ms",
            LONG_POLL_MS,
        ],
    )
}

fn events_watch(h: &Harness) -> Watcher {
    create_session(h, "boot");
    let from = next_seq(h).to_string();
    Watcher::spawn(
        h,
        &[
            "events",
            "watch",
            "--filter",
            "session.created",
            "--from-seq",
            &from,
            "--timeout-ms",
            LONG_POLL_MS,
        ],
    )
}

/// Issue 134: a piped `events watch` hands each event to its reader as it
/// happens, as one JSON line.
#[test]
fn piped_events_watch_delivers_an_event_without_waiting_out_the_long_poll() {
    let h = Harness::new();
    let mut w = events_watch(&h);
    let rx = w.output();
    create_session(&h, "piped-134");
    let out = read_until(&rx, "\n", "events watch", w.before_the_long_poll_ends());
    let event: serde_json::Value =
        serde_json::from_str(out.lines().next().unwrap()).expect("JSON line");
    assert_eq!(event["type"], "session.created", "{out}");
    assert_eq!(event["data"]["data"]["name"], "piped-134", "{out}");
}

/// Closing the read end (`| head -n 1`) ends the watch with status 0 — not a
/// broken-pipe panic.
#[test]
fn events_watch_ends_cleanly_when_its_reader_closes_the_pipe() {
    let h = Harness::new();
    let mut w = events_watch(&h);
    let rx = w.output();
    create_session(&h, "first");
    read_until(&rx, "\n", "events watch", w.before_the_long_poll_ends());
    drop(rx);

    let n = std::cell::Cell::new(0);
    let (status, stderr) = w.exit_after_reader_closed(|| {
        n.set(n.get() + 1);
        create_session(&h, &format!("poke-{}", n.get()));
    });
    assert!(
        status.success() && !stderr.contains("panicked"),
        "`events watch` into a closed pipe exited {status}; stderr: {stderr}"
    );
}

/// `pane watch` streams a pane's raw output, colour intact, as it is written,
/// and stops once its reader is gone. The pane writes once a second, so a
/// poll that waited for its 50-chunk page would outlast the long poll.
#[test]
fn pane_watch_streams_promptly_and_ends_cleanly_when_its_reader_closes_the_pipe() {
    let h = Harness::new();
    let out = h.cli(&[
        "session",
        "create",
        "talker",
        "-d",
        "--",
        "sh",
        "-c",
        "while :; do printf '\\033[38;2;255;0;128mTRUE\\033[0m \\033[38;5;208mIDX\\033[0m \\033[31mBASIC\\033[0m tick\\n'; sleep 1; done",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut w = pane_watch(&h, "talker", &pane_id_of(&h, "talker"));
    let rx = w.output();
    let seen = read_until(&rx, "tick", "pane watch", w.before_the_long_poll_ends());
    for sgr in ["\x1b[38;2;255;0;128m", "\x1b[38;5;208m", "\x1b[31m"] {
        assert!(seen.contains(sgr), "colour {sgr:?} missing from {seen:?}");
    }
    drop(rx);

    let (status, stderr) = w.exit_after_reader_closed(|| {});
    assert!(
        status.success() && !stderr.contains("panicked"),
        "`pane watch` into a closed pipe exited {status}; stderr: {stderr}"
    );
}

/// Raw output with no newline is still pending in stdout's line buffer when
/// the reader closes; ending the watch must not write it again.
#[test]
fn pane_watch_of_unterminated_output_ends_cleanly_when_its_reader_closes_the_pipe() {
    let h = Harness::new();
    let out = h.cli(&[
        "session",
        "create",
        "dots",
        "-d",
        "--",
        "sh",
        "-c",
        "while :; do printf '\\033[38;2;255;0;128mT\\033[38;5;208mI\\033[31mB\\033[0mdot'; sleep 1; done",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut w = pane_watch(&h, "dots", &pane_id_of(&h, "dots"));
    let rx = w.output();
    read_until(&rx, "dot", "pane watch", w.before_the_long_poll_ends());
    drop(rx);

    let (status, stderr) = w.exit_after_reader_closed(|| {});
    assert!(
        status.success() && !stderr.contains("panicked"),
        "`pane watch` of unterminated output into a closed pipe exited {status}; stderr: {stderr}"
    );
}

/// A pane that answers a line and then goes quiet: its last write reaches the
/// watcher though nothing follows it. The reply is two writes 50 ms apart, so
/// the second lands inside the 100 ms sampling window the first one opened.
#[test]
fn pane_watch_delivers_a_last_write_that_no_later_output_follows() {
    let h = Harness::new();
    let out = h.cli(&[
        "session",
        "create",
        "quiet",
        "-d",
        "--",
        "sh",
        "-c",
        "(while :; do echo waiting; sleep 0.3; done) & t=$!; read -r x; kill $t; sleep 0.3; \
         printf 'first %s\\n' \"$x\"; sleep 0.05; \
         printf '\\033[38;2;0;200;80mTRUE\\033[0m \\033[38;5;33mIDX\\033[0m \\033[32mBASIC\\033[0m last-%s\\n' \"$x\"; \
         exec sleep 300",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let pane = pane_id_of(&h, "quiet");
    let mut w = pane_watch(&h, "quiet", &pane);
    let rx = w.output();
    let by = w.before_the_long_poll_ends();
    read_until(&rx, "waiting", "pane watch before input", by);
    h.send_line_token(&pane, "ping");
    // `last-ping` is printed only by the reply; the echoed line reads `ping`.
    let seen = read_until(&rx, "last-ping", "pane watch after its pane went quiet", by);
    for sgr in ["\x1b[38;2;0;200;80m", "\x1b[38;5;33m", "\x1b[32m"] {
        assert!(seen.contains(sgr), "colour {sgr:?} missing from {seen:?}");
    }
}
