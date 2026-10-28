//! `shux events watch` and `shux pane watch` are streams meant for a pipe:
//! each item reaches the reader as it happens, and the command ends cleanly
//! when the reader closes the pipe.
//!
//! These drive the REAL `shux` binary against a REAL daemon, with stdout a
//! real pipe — the case a terminal-attached run never exercises.

use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

mod lens_common;
use lens_common::*;

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

/// Issue 134: a piped `events watch` must hand each event to its reader as it
/// happens, not hold it until the 5 s long-poll deadline.
#[test]
fn piped_events_watch_delivers_an_event_without_waiting_out_the_long_poll() {
    let h = Harness::new();
    create_session(&h, "boot");
    let from = next_seq(&h).to_string();
    let mut w = Watcher::spawn(
        &h,
        &[
            "events",
            "watch",
            "--filter",
            "session.created",
            "--from-seq",
            &from,
        ],
    );
    let (tx, rx) = mpsc::channel();
    let stdout = w.0.stdout.take().expect("stdout");
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let created = Instant::now();
    create_session(&h, "piped-134");
    let line = rx.recv_timeout(Duration::from_secs(4)).unwrap_or_else(|_| {
        panic!(
            "no event reached the pipe within 4 s of creating a session \
                 (it took {:?} so far); the default long-poll is 5 s",
            created.elapsed()
        )
    });
    let event: serde_json::Value = serde_json::from_str(&line).expect("JSON line");
    assert_eq!(event["type"], "session.created", "{line}");
    assert_eq!(event["data"]["data"]["name"], "piped-134", "{line}");
}

/// Closing the read end (`| head -n 1`) ends the watch with status 0 — not a
/// broken-pipe panic.
#[test]
fn events_watch_ends_cleanly_when_its_reader_closes_the_pipe() {
    let h = Harness::new();
    create_session(&h, "boot");
    let from = next_seq(&h).to_string();
    let mut w = Watcher::spawn(&h, &["events", "watch", "--from-seq", &from]);
    create_session(&h, "first");
    let mut reader = BufReader::new(w.0.stdout.take().expect("stdout"));
    let mut line = String::new();
    reader.read_line(&mut line).expect("read the first event");
    assert!(line.contains("session.created"), "{line}");
    drop(reader);

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

/// The same for `pane watch`, which streams a pane's raw output. It used to
/// discard the write error and poll forever for a reader that was gone.
#[test]
fn pane_watch_ends_cleanly_when_its_reader_closes_the_pipe() {
    let h = Harness::new();
    let out = h.cli(&[
        "session",
        "create",
        "talker",
        "-d",
        "--",
        "sh",
        "-c",
        "while :; do printf '\\033[38;2;255;0;128mTRUE\\033[0m \\033[38;5;208mIDX\\033[0m \\033[31mBASIC\\033[0m tick\\n'; sleep 0.05; done",
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
            "500",
        ],
    );
    let mut stdout = w.0.stdout.take().expect("stdout");
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut buf = [0u8; 4096];
    while !String::from_utf8_lossy(&seen).contains("tick") {
        assert!(
            Instant::now() < deadline,
            "pane watch delivered nothing: {seen:?}"
        );
        let n = stdout.read(&mut buf).expect("read pane watch");
        assert!(n > 0, "pane watch closed its stdout early");
        seen.extend_from_slice(&buf[..n]);
    }
    let seen = String::from_utf8_lossy(&seen);
    for sgr in ["\x1b[38;2;255;0;128m", "\x1b[38;5;208m", "\x1b[31m"] {
        assert!(seen.contains(sgr), "colour {sgr:?} missing from {seen:?}");
    }
    drop(stdout);

    let (status, stderr) = w.exit_after_reader_closed(|| {});
    assert!(
        status.success() && !stderr.contains("panicked"),
        "`pane watch` into a closed pipe exited {status}; stderr: {stderr}"
    );
}
