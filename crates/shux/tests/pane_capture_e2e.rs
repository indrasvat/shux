//! End-to-end: what a real pane's program writes, as `pane capture` reports it.
//!
//! Each scene runs a program in a pane of the shipped `shux` binary, in an
//! isolated `XDG_RUNTIME_DIR`, and reads it back the way an operator or an
//! agent would. The VT-level behaviour has its own tests in `shux-vt`; these
//! check that it survives the daemon's PTY, its pane I/O and the CLI.
//!
//! Every captured scene draws a colour probe (truecolor, 256-indexed and basic
//! ANSI) first, so a run that lost colour handling cannot pass on characters.

use std::path::PathBuf;
use std::process::{Command, Output};

const COLS: usize = 80;
const ROWS: usize = 24;

const COLOUR_PROBE: &str = "\\033[38;2;120;220;180mTRUECOLOR\\033[0m \\033[38;5;208mINDEXED\\033[0m \\033[34mBASIC\\033[0m";

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

    fn work(&self, name: &str) -> PathBuf {
        self.root.path().join("work").join(name)
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(&self.bin)
            .env("XDG_RUNTIME_DIR", self.root.path().join("runtime"))
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env_remove("SHUX_SOCKET")
            .env("NO_COLOR", "1")
            .env("SHELL", "/bin/sh")
            .current_dir(self.root.path().join("work"))
            .args(args)
            .output()
            .expect("spawn shux")
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

    /// A pane running `script` under `sh` at COLS x ROWS; the script starts only
    /// once the pane has that size, and the pane stays up afterwards.
    #[track_caller]
    fn pane_running(&mut self, session: &str, script: &str) -> String {
        let go = self.work(&format!("{session}.sized"));
        let body = format!(
            "while [ ! -e '{}' ]; do sleep 0.05; done\nprintf '{COLOUR_PROBE}\\n'\n{script}\nexec sleep 900\n",
            go.display()
        );
        let path = self.work(&format!("{session}.sh"));
        std::fs::write(&path, body).expect("write script");
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
        let (cols, rows) = (COLS.to_string(), ROWS.to_string());
        self.ok(&[
            "pane", "set-size", "-s", session, "-p", &pane, "--cols", &cols, "--rows", &rows,
        ]);
        std::fs::write(go, b"").expect("write gate file");
        pane
    }

    /// A deadline for reporting failure, not an assertion: the screen is
    /// checked right after it.
    #[track_caller]
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
            "60000",
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

    /// Wait for `marker`, then capture, and check the colour probe came through.
    #[track_caller]
    fn captured_after(&self, session: &str, pane: &str, marker: &str) -> String {
        self.wait_for(session, pane, marker);
        let text = self.capture(session, pane);
        for probe in ["TRUECOLOR", "INDEXED", "BASIC"] {
            assert!(
                text.contains(probe),
                "colour probe {probe} missing from:\n{text}"
            );
        }
        text
    }

    /// Run `program` (a Python source) as the pane's program after the probe.
    #[track_caller]
    fn pane_running_python(&mut self, session: &str, program: &str) -> String {
        let path = self.work(&format!("{session}.py"));
        std::fs::write(&path, program).expect("write probe");
        self.pane_running(session, &format!("python3 '{}'", path.display()))
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        for session in std::mem::take(&mut self.sessions) {
            let _ = self.run(&["session", "kill", &session]);
        }
        let _ = self.run(&["daemon", "stop"]);
        let pidfile = self.root.path().join("runtime/shux/shux.pid");
        if let Some(pid) = std::fs::read_to_string(&pidfile)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok())
            && !shux_testkit::terminate(pid)
            && !std::thread::panicking()
        {
            panic!("daemon {pid} outlived the test");
        }
    }
}

#[test]
fn a_capture_keeps_every_grapheme_cluster_whole() {
    let mut env = Env::new();
    let pane = env.pane_running_python(
        "graph",
        "import sys\nsys.stdout.write('e\\u0301 \\U0001f6e0\\ufe0f \\U0001f44d\\U0001f3fd \
         \\U0001f468\\u200d\\U0001f4bb \\U0001f1fa\\U0001f1f8 GRAPH' + 'EMES-DONE\\n')\n",
    );
    let text = env.captured_after("graph", &pane, "GRAPHEMES-DONE");
    for cluster in ["e\u{0301}", "🛠\u{fe0f}", "👍🏽", "👨\u{200d}💻", "🇺🇸"] {
        assert!(
            text.contains(cluster),
            "capture split or lost {cluster:?}:\n{text}"
        );
    }
}

#[test]
fn a_capture_shows_dec_special_graphics_as_box_drawing() {
    let mut env = Env::new();
    let pane = env.pane_running("dec", "printf '\\033(0lqkxmj\\033(B DEC-''DONE\\n'");
    let text = env.captured_after("dec", &pane, "DEC-DONE");
    assert!(
        text.contains("┌─┐│└┘ DEC-DONE"),
        "line-drawing set not translated:\n{text}"
    );
}

#[test]
fn a_capture_honours_tab_stops_the_program_sets_and_clears() {
    let mut env = Env::new();
    let pane = env.pane_running_python(
        "tabs",
        r"import sys
sys.stdout.write('default:\tA\tB\n')
sys.stdout.write('\x1b[13G\x1bH\rcustom:\tA\tB\tC\n')
sys.stdout.write('\x1b[9G\x1b[g\rclear-current:\tA\n')
sys.stdout.write('\x1b[3g\rclear-all:\tZ\n')
sys.stdout.write('TABS-' + 'DONE\n')
",
    );
    let text = env.captured_after("tabs", &pane, "TABS-DONE");
    assert!(
        text.contains("default:        A       B"),
        "default stops:\n{text}"
    );
    assert!(
        text.contains("custom: A   B   C"),
        "HTS at column 13:\n{text}"
    );
    assert!(
        text.contains("clear-current:  A"),
        "TBC 0 at column 9:\n{text}"
    );
    let clear_all = text
        .lines()
        .find(|l| l.starts_with("clear-all:"))
        .unwrap_or("");
    assert_eq!(
        clear_all.chars().nth(COLS - 1),
        Some('Z'),
        "TBC 3 must send HT to the last column:\n{text}"
    );
}

/// The program asks for the cursor position, the synchronized-output mode and
/// the terminal's version, then reports what came back on its own stdin.
#[test]
fn terminal_queries_get_their_replies_on_the_programs_stdin() {
    let mut env = Env::new();
    let pane = env.pane_running_python(
        "query",
        r#"import os, select, sys, termios, time, tty
fd = 0
saved = termios.tcgetattr(fd)
tty.setraw(fd)
want = (b"\x1b[5;10R", b"\x1b[?2026;2$y", b"\x1bP>|shux ")
data = b""
try:
    os.write(1, b"\x1b[5;10H\x1b[6n\x1b[?2026$p\x1b[>0q")
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline and not all(w in data for w in want):
        if select.select([fd], [], [], 0.2)[0]:
            data += os.read(fd, 256)
finally:
    termios.tcsetattr(fd, termios.TCSADRAIN, saved)
sys.stdout.write("\r\n" + "".join("Y" if w in data else "N" for w in want) + " QUERIES-" + "DONE\r\n")
sys.stdout.flush()
"#,
    );
    let text = env.captured_after("query", &pane, "QUERIES-DONE");
    assert!(
        text.contains("YYY QUERIES-DONE"),
        "CPR, DECRQM ?2026 and XTVERSION each answered (Y) or not (N):\n{text}"
    );
}

/// The program holds a synchronized-output frame open until the test has
/// captured. It reports ready only once shux has answered a cursor report
/// queued behind the hidden frame, so the VT has parsed that frame; both ends
/// are ordered, and no timing decides the outcome.
#[test]
fn a_capture_during_synchronized_output_shows_the_frame_before_it() {
    let mut env = Env::new();
    let (ready, release) = (env.work("sync.ready"), env.work("sync.release"));
    let pane = env.pane_running_python(
        "sync",
        &format!(
            r#"import os, pathlib, termios, time, tty
fd = 0
saved = termios.tcgetattr(fd)
tty.setraw(fd)
try:
    os.write(1, b"OLD-FRAME")
    os.write(1, b"\x1b[?2026h\x1b[1;1HNEW-FRAME\x1b[6n")
    reply = b""
    while not reply.endswith(b"R"):
        reply += os.read(fd, 64)
    pathlib.Path({ready:?}).write_text("ready")
    release = pathlib.Path({release:?})
    deadline = time.monotonic() + 60
    while not release.exists() and time.monotonic() < deadline:
        time.sleep(0.01)
    os.write(1, b"\x1b[?2026l")
finally:
    termios.tcsetattr(fd, termios.TCSADRAIN, saved)
"#,
            ready = ready.display().to_string(),
            release = release.display().to_string(),
        ),
    );
    env.wait_for("sync", &pane, "OLD-FRAME");
    let r = &ready;
    assert!(
        shux_testkit::poll_until(shux_testkit::BUDGET, || r.exists()),
        "the program never had its cursor report answered:\n{}",
        env.capture("sync", &pane)
    );
    let frozen = env.capture("sync", &pane);
    std::fs::write(&release, b"go").expect("release the frame");
    assert!(
        !frozen.contains("NEW-FRAME"),
        "the pending frame leaked:\n{frozen}"
    );
    for kept in ["TRUECOLOR", "INDEXED", "BASIC", "OLD-FRAME"] {
        assert!(
            frozen.contains(kept),
            "the frame before the sync lost {kept}:\n{frozen}"
        );
    }
    env.wait_for("sync", &pane, "NEW-FRAME");
}
