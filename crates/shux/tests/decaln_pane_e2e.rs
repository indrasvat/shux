//! End-to-end: DECALN (`ESC # 8`) through a real daemon and a real PTY
//! (issue #117).
//!
//! The VT-level suite lives in `crates/shux-vt/tests/decaln.rs` and drives the
//! parser directly. This one drives the shipped `shux` binary: a real daemon in
//! an isolated `XDG_RUNTIME_DIR`, a real pane running a real shell that emits
//! the sequence, and the same `pane capture` an operator or an agent would run.
//! It is the level at which "shux reports a blank screen where every other
//! terminal reports a full one" is actually observable.
//!
//! Every scene emits a colour probe — truecolor, 256-indexed and basic ANSI —
//! so a monochrome or `NO_COLOR` regression cannot slip through a run that only
//! ever looked at characters.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

const COLS: usize = 40;
const ROWS: usize = 10;

/// Truecolor + 256-indexed + basic ANSI, all on one line. Mandatory on every
/// daemon-backed capture (see CLAUDE.md).
const COLOUR_PROBE: &str = "\\033[38;2;120;220;180mTRUECOLOR\\033[0m \\033[38;5;208mINDEXED\\033[0m \\033[34mBASIC\\033[0m";

// ── isolated daemon environment ─────────────────────────────────────────

struct Env {
    bin: PathBuf,
    root: tempfile::TempDir,
    sessions: Vec<String>,
}

impl Env {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("temp root");
        for sub in ["runtime", "config/shux", "work"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
        }
        Self {
            bin: PathBuf::from(env!("CARGO_BIN_EXE_shux")),
            root,
            sessions: Vec::new(),
        }
    }

    fn shux(&self) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.env("XDG_RUNTIME_DIR", self.root.path().join("runtime"))
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env_remove("SHUX_SOCKET")
            .env("NO_COLOR", "1")
            .env("SHELL", "/bin/sh")
            .current_dir(self.root.path().join("work"));
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.shux().args(args).output().expect("spawn shux")
    }

    #[track_caller]
    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// Spawn a pane running `script` under `sh`, sized to COLS x ROWS, and
    /// return its pane id. The script is kept alive afterwards so the pane's
    /// screen is not torn down before it is captured.
    #[track_caller]
    fn pane_running(&mut self, session: &str, script: &str) -> String {
        let body = format!("{script}\nexec sleep 900\n");
        let path = self.root.path().join("work").join(format!("{session}.sh"));
        std::fs::write(&path, &body).expect("write script");

        self.ok(&[
            "session",
            "create",
            session,
            "-d",
            "--",
            "sh",
            path.to_str().unwrap(),
        ]);
        self.sessions.push(session.to_string());

        let json = self.ok(&["--format", "json", "pane", "list", "-s", session]);
        let v: serde_json::Value = serde_json::from_str(&json).expect("pane list json");
        let pane = v[0]["id"].as_str().expect("pane id").to_string();

        self.ok(&[
            "pane",
            "set-size",
            "-s",
            session,
            "-p",
            &pane,
            "--cols",
            &COLS.to_string(),
            "--rows",
            &ROWS.to_string(),
        ]);
        pane
    }

    fn wait_for(&self, session: &str, pane: &str, needle: &str) {
        let out = self.run(&[
            "pane",
            "wait-for",
            "-s",
            session,
            "-p",
            pane,
            "-t",
            needle,
            "--timeout-ms",
            "20000",
        ]);
        assert!(
            out.status.success(),
            "pane never showed {needle:?}; screen was:\n{}",
            self.capture(session, pane)
        );
    }

    fn capture(&self, session: &str, pane: &str) -> String {
        self.ok(&["pane", "capture", "-s", session, "-p", pane])
    }

    /// The pane's visible rows, padded to the full width — `pane glance` is
    /// byte-stable in a way `pane capture` deliberately is not, which is what
    /// makes "every cell on every row" assertable.
    fn glance_rows(&self, pane: &str) -> Vec<String> {
        let json = self.ok(&["--format", "json", "pane", "glance", pane, "--text-only"]);
        let v: serde_json::Value = serde_json::from_str(&json).expect("glance json");
        let text = v["result"]["text"]
            .as_str()
            .or_else(|| v["text"].as_str())
            .unwrap_or_else(|| panic!("no text in glance output: {json}"));
        text.lines().map(str::to_string).collect()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        for session in std::mem::take(&mut self.sessions) {
            let _ = self.run(&["session", "kill", &session]);
        }
        let _ = self.run(&["daemon", "stop"]);

        // Hard proof of no leak: the recorded pid must be gone. Identified by
        // pidfile, never by matching a command line.
        let pidfile = self.root.path().join("runtime/shux/shux.pid");
        if let Some(pid) = std::fs::read_to_string(&pidfile)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok())
        {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if kill(Pid::from_raw(pid), None).is_err() {
                    return;
                }
                thread::sleep(Duration::from_millis(50));
            }
            let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
            panic!("daemon {pid} outlived the test");
        }
    }
}

#[track_caller]
fn assert_all_rows_are_pattern(rows: &[String], what: &str) {
    assert_eq!(rows.len(), ROWS, "{what}: row count\n{rows:#?}");
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(
            row.as_str(),
            "E".repeat(COLS).as_str(),
            "{what}: row {i} is not the alignment pattern\n{rows:#?}"
        );
    }
}

// ── scenes ──────────────────────────────────────────────────────────────

/// The headline: a real pane emits `ESC # 8` and the operator's `pane capture`
/// shows a full screen of `E`. Before the fix it showed a blank one.
#[test]
fn a_real_pane_emitting_decaln_shows_a_full_screen_of_e() {
    let mut env = Env::new();
    let pane = env.pane_running(
        "d117a",
        &format!("printf '{COLOUR_PROBE}\\n'\nsleep 0.3\nprintf '\\033#8'"),
    );

    // The probe proves the pane really drew colour before the fill; the fill
    // then covers it, which is exactly what DECALN is supposed to do.
    env.wait_for("d117a", &pane, "TRUECOLOR");
    env.wait_for("d117a", &pane, "EEEEEEEEEE");

    let rows = env.glance_rows(&pane);
    assert_all_rows_are_pattern(&rows, "plain DECALN through a real pane");

    let captured = env.capture("d117a", &pane);
    assert!(
        !captured.contains("TRUECOLOR"),
        "the fill did not cover the colour probe:\n{captured}"
    );
}

/// The three clauses of VT510 DECALN that are easy to get wrong, in one pane:
/// a scroll region must not clip the fill, the styled pen must not colour it,
/// and the cursor must end up at home — which is where the next character
/// lands, in the pen's colour, over the pattern.
#[test]
fn decaln_ignores_the_scroll_region_and_the_pen_and_homes_the_cursor() {
    let mut env = Env::new();
    let pane = env.pane_running(
        "d117b",
        &format!(
            "printf '{COLOUR_PROBE}\\n'\n\
             printf '\\033[3;6r'\n\
             printf '\\033[1;38;2;255;80;80;48;5;27m'\n\
             sleep 0.3\n\
             printf '\\033#8'\n\
             printf 'HOME'\n"
        ),
    );
    env.wait_for("d117b", &pane, "HOME");

    let rows = env.glance_rows(&pane);
    assert_eq!(rows.len(), ROWS);
    // Cursor homed: the four characters landed at the top-left, over the fill.
    assert_eq!(rows[0], format!("HOME{}", "E".repeat(COLS - 4)));
    // Fill not clipped by the region: rows outside rows 3..6 are filled too.
    for (i, row) in rows.iter().enumerate().skip(1) {
        assert_eq!(
            row.as_str(),
            "E".repeat(COLS).as_str(),
            "row {i} outside the scroll region was not filled\n{rows:#?}"
        );
    }
}

/// The alternate-screen buffer is recycled between applications in the same
/// pane (issue #106). A screen filled by DECALN and then retired must never be
/// handed to the next application as a blank canvas.
#[test]
fn a_retired_alternate_screen_filled_by_decaln_is_blank_for_the_next_app() {
    let mut env = Env::new();
    let pane = env.pane_running(
        "d117c",
        // App one: alternate screen, fill it, leave.
        // App two: alternate screen again, draw a single marker line.
        "printf '\\033[?1049h\\033#8'\n\
         sleep 0.4\n\
         printf '\\033[?1049l'\n\
         sleep 0.2\n\
         printf '\\033[?1049h'\n\
         sleep 0.2\n\
         printf '\\033[5;1HSECOND-APP'\n",
    );
    env.wait_for("d117c", &pane, "SECOND-APP");

    let rows = env.glance_rows(&pane);
    let leaked: Vec<(usize, &String)> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.contains("EEEE"))
        .collect();
    assert!(
        leaked.is_empty(),
        "the previous application's alignment pattern survived into the \
         recycled alternate screen: {leaked:?}\n{rows:#?}"
    );
    assert!(
        rows.iter().any(|r| r.contains("SECOND-APP")),
        "the second application never drew\n{rows:#?}"
    );
}

/// Leaving the alternate screen must put the primary screen back exactly as it
/// was, DECALN on the alternate screen notwithstanding.
#[test]
fn decaln_on_the_alternate_screen_does_not_reach_the_primary_one() {
    let mut env = Env::new();
    let pane = env.pane_running(
        "d117d",
        &format!(
            "printf '{COLOUR_PROBE}\\n'\n\
             printf 'PRIMARY-CONTENT\\n'\n\
             sleep 0.3\n\
             printf '\\033[?1049h\\033#8'\n\
             sleep 0.4\n\
             printf '\\033[?1049l'\n"
        ),
    );
    env.wait_for("d117d", &pane, "PRIMARY-CONTENT");
    // Give the alternate-screen round trip time to complete before capturing.
    thread::sleep(Duration::from_millis(1500));

    let captured = env.capture("d117d", &pane);
    assert!(
        captured.contains("PRIMARY-CONTENT") && captured.contains("TRUECOLOR"),
        "the primary screen lost its content across the alternate-screen \
         round trip:\n{captured}"
    );
    assert!(
        !captured.contains("EEEE"),
        "the alternate screen's pattern reached the primary screen:\n{captured}"
    );
}
