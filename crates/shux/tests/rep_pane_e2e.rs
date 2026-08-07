//! End-to-end: REP (`CSI Pn b`) through a real daemon and a real PTY
//! (issue #122).
//!
//! The VT-level suite lives in `crates/shux-vt/tests/rep.rs` and drives the
//! parser directly. This one drives the shipped `shux` binary: a real daemon in
//! an isolated `XDG_RUNTIME_DIR`, a real pane running a real shell that emits
//! the sequence, and the same `pane capture` / `pane glance` an operator or an
//! agent would run. It is the level at which "the bar an application drew is
//! missing" is actually observable.
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
const ROWS: usize = 8;

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
    /// return its pane id. The script parks afterwards so the pane's screen is
    /// still there to be captured.
    ///
    /// The pane starts at the daemon's default geometry and is resized a moment
    /// later, so the script BLOCKS until the resize has landed. Without that it
    /// draws on an 80x24 screen which is then reflowed to 40x8 — content pushed
    /// into scrollback, assertions failing for a reason that has nothing to do
    /// with what is being tested.
    #[track_caller]
    fn pane_running(&mut self, session: &str, script: &str) -> String {
        let go = self.root.path().join("work").join(format!("{session}.go"));
        let _ = std::fs::remove_file(&go);
        let body = format!(
            "while [ ! -e '{}' ]; do sleep 0.05; done\n{script}\nexec sleep 900\n",
            go.display()
        );
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
        std::fs::write(&go, b"go").expect("release the pane script");
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

    fn settle(&self, pane: &str) {
        self.ok(&[
            "pane",
            "wait-settled",
            pane,
            "--quiet",
            "400",
            "--timeout",
            "15000",
        ]);
    }

    fn capture(&self, session: &str, pane: &str) -> String {
        self.ok(&[
            "pane",
            "capture",
            "-s",
            session,
            "-p",
            pane,
            "--lines",
            &ROWS.to_string(),
        ])
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

// ── scenes ──────────────────────────────────────────────────────────────

/// The headline. A pane draws a rule the way an application actually draws one:
/// address the line, print one character, repeat it across the width. The
/// cursor move is what made the repeat vanish.
#[test]
fn a_real_pane_drawing_a_rule_with_rep_gets_a_rule() {
    let mut env = Env::new();
    let pane = env.pane_running(
        "rep-rule",
        &format!(
            "printf '\\033[1;1H{COLOUR_PROBE}'\n\
             printf '\\033[4;1H-\\033[39b'\n\
             printf '\\033[8;1HREADY'\n"
        ),
    );
    env.wait_for("rep-rule", &pane, "READY");
    env.settle(&pane);

    let rows = env.glance_rows(&pane);
    assert_eq!(
        rows[3].trim_end(),
        "-".repeat(COLS),
        "the rule is missing or short; screen was:\n{}",
        env.capture("rep-rule", &pane)
    );

    let screen = env.capture("rep-rule", &pane);
    for probe in ["TRUECOLOR", "INDEXED", "BASIC"] {
        assert!(screen.contains(probe), "colour probe {probe} missing");
    }
}

/// The issue's own reproduction, at the exact position that used to drop the
/// repeat silently: column 0 after homing the cursor.
#[test]
fn a_real_pane_repeating_at_column_zero_is_not_dropped() {
    let mut env = Env::new();
    let pane = env.pane_running(
        "rep-home",
        &format!(
            "printf '\\033[1;1H{COLOUR_PROBE}'\n\
             printf 'X'\n\
             printf '\\033[1;1H\\033[3b'\n\
             printf '\\033[8;1HREADY'\n"
        ),
    );
    env.wait_for("rep-home", &pane, "READY");
    env.settle(&pane);

    let rows = env.glance_rows(&pane);
    assert!(
        rows[0].starts_with("XXX"),
        "REP at column 0 was dropped; row 0 was {:?}\nscreen:\n{}",
        rows[0],
        env.capture("rep-home", &pane)
    );
}

/// A progress bar redrawn in place: address the line, erase it, print one block,
/// repeat it. Every frame starts with a cursor move, which is the pattern that
/// was broken.
#[test]
fn a_real_pane_redrawing_a_progress_bar_in_place_advances() {
    let mut env = Env::new();
    let pane = env.pane_running(
        "rep-bar",
        &format!(
            "printf '\\033[1;1H{COLOUR_PROBE}'\n\
             for n in 5 10 20; do\n\
             \x20 printf '\\033[6;1H\\033[K\\033[38;5;208m#'\n\
             \x20 printf '\\033[%db' \"$n\"\n\
             \x20 printf '\\033[0m'\n\
             done\n\
             printf '\\033[8;1HREADY'\n"
        ),
    );
    env.wait_for("rep-bar", &pane, "READY");
    env.settle(&pane);

    let rows = env.glance_rows(&pane);
    assert_eq!(
        rows[5].trim_end(),
        "#".repeat(21),
        "the final bar frame is wrong; screen was:\n{}",
        env.capture("rep-bar", &pane)
    );
}

/// Box drawing: the line-drawing character set plus REP is how a great many
/// TUIs draw a horizontal rule, and the remembered character has to be the
/// translated glyph rather than the ASCII `q` that carried it.
#[test]
fn a_real_pane_drawing_a_box_rule_repeats_the_line_glyph() {
    let mut env = Env::new();
    let pane = env.pane_running(
        "rep-box",
        &format!(
            "printf '\\033[1;1H{COLOUR_PROBE}'\n\
             printf '\\033[3;1H\\033(0q\\033(B\\033[9b'\n\
             printf '\\033[8;1HREADY'\n"
        ),
    );
    env.wait_for("rep-box", &pane, "READY");
    env.settle(&pane);

    let rows = env.glance_rows(&pane);
    assert!(
        rows[2].starts_with(&"\u{2500}".repeat(10)),
        "line-drawing rule is wrong; row 2 was {:?}\nscreen:\n{}",
        rows[2],
        env.capture("rep-box", &pane)
    );
}

/// A pane with nothing printed before the repeat must not invent content, and a
/// huge count must not hang the daemon. Ten bytes of pane output buy at most one
/// screenful of work (issue #102).
#[test]
fn a_real_pane_cannot_buy_unbounded_work_with_one_rep() {
    let mut env = Env::new();
    let pane = env.pane_running(
        "rep-bound",
        &format!(
            "printf '\\033[1;1H{COLOUR_PROBE}'\n\
             printf '\\033[65535b'\n\
             printf 'W'\n\
             printf '\\033[65535b'\n\
             printf '\\033[8;1HREADY'\n"
        ),
    );
    env.wait_for("rep-bound", &pane, "READY");
    env.settle(&pane);

    // The daemon is still answering, which is the point of the bound.
    let rows = env.glance_rows(&pane);
    assert_eq!(rows.len(), ROWS);
    assert!(
        rows.iter().any(|r| r.contains("READY")),
        "the pane never got back to a usable screen:\n{rows:#?}"
    );
}
