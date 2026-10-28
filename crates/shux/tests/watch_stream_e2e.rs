//! `shux events watch` and `shux pane watch` are streams meant for a pipe:
//! each item reaches the reader as it happens, and the command ends cleanly
//! when the reader closes the pipe.
//!
//! These drive the REAL `shux` binary against a REAL daemon, with stdout a
//! real pipe — the case a terminal-attached run never exercises. Each watch
//! runs with a 30 s long poll, so an item held until the deadline cannot land
//! inside the 10 s the tests allow.

use std::io::Read;
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

mod lens_common;
use lens_common::*;

const LONG_POLL_MS: &str = "30000";
const PROMPT: Duration = Duration::from_secs(10);

/// Kills the watcher if an assertion fails first, so no client outlives the test.
struct Watcher(Child);

impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Watcher {
    fn spawn(h: &Harness, args: &[&str]) -> Self {
        Self(
            h.shux()
                .args(args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap_or_else(|e| panic!("spawn shux {args:?}: {e}")),
        )
    }

    /// The watcher's stdout as chunks read on another thread, so a test waits
    /// on it with a timeout instead of blocking in `read`. Dropping the
    /// receiver closes the pipe at the next chunk.
    fn output(&mut self) -> mpsc::Receiver<Vec<u8>> {
        let mut stdout = self.0.stdout.take().expect("stdout");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = stdout.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        rx
    }

    /// Wait for the watcher to exit on its own, nudging it with `poke` so it
    /// has something to write into the closed pipe. Returns status and stderr.
    fn exit_after_reader_closed(mut self, poke: impl Fn()) -> (std::process::ExitStatus, String) {
        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            if let Some(st) = self.0.try_wait().expect("try_wait") {
                break st;
            }
            assert!(
                Instant::now() < deadline,
                "the watcher was still running 15 s after its reader closed the pipe"
            );
            poke();
            std::thread::sleep(Duration::from_millis(200));
        };
        let mut stderr = String::new();
        if let Some(mut e) = self.0.stderr.take() {
            let _ = e.read_to_string(&mut stderr);
        }
        (status, stderr)
    }
}

/// Collect output until it holds `needle`, failing after `PROMPT`.
fn read_until(rx: &mpsc::Receiver<Vec<u8>>, needle: &str, what: &str) -> String {
    let started = Instant::now();
    let mut seen = Vec::new();
    while !String::from_utf8_lossy(&seen).contains(needle) {
        let left = PROMPT.saturating_sub(started.elapsed());
        match rx.recv_timeout(left) {
            Ok(chunk) => seen.extend_from_slice(&chunk),
            Err(e) => panic!(
                "{what}: no {needle:?} within {PROMPT:?} ({e}); saw {:?}",
                String::from_utf8_lossy(&seen)
            ),
        }
    }
    String::from_utf8_lossy(&seen).into_owned()
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
    let out = read_until(&rx, "\n", "events watch");
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
    read_until(&rx, "\n", "events watch");
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
/// and stops once its reader is gone. The pane writes twice a second, so a
/// poll that waited for its 50-chunk page would take 25 s.
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
        "while :; do printf '\\033[38;2;255;0;128mTRUE\\033[0m \\033[38;5;208mIDX\\033[0m \\033[31mBASIC\\033[0m tick\\n'; sleep 0.5; done",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let sessions = h.rpc_ok("session.list", serde_json::json!({}));
    let pane = sessions["sessions"]
        .as_array()
        .and_then(|a| a.iter().find(|s| s["name"] == "talker"))
        .and_then(|s| s["pane_id"].as_str())
        .unwrap_or_else(|| panic!("no talker pane in {sessions}"))
        .to_string();

    let mut w = Watcher::spawn(
        &h,
        &[
            "pane",
            "watch",
            "-s",
            "talker",
            "-p",
            &pane,
            "--timeout-ms",
            LONG_POLL_MS,
        ],
    );
    let rx = w.output();
    let seen = read_until(&rx, "tick", "pane watch");
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
