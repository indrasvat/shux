//! End-to-end: what a real pane's program writes, read back through the daemon.
//!
//! The VT behaviour itself is tested in `shux-vt`. These cover what only the
//! daemon's path can break: non-ASCII text through PTY, RPC and CLI, terminal
//! replies written back to the program, and a capture while synchronized output
//! holds the frame.

mod pane_e2e;

use pane_e2e::{COLOUR_PROBE, Env};

/// A pane running a Python `program` after the colour probe.
#[track_caller]
fn pane_running_python(env: &mut Env, session: &str, program: &str) -> String {
    let path = env.work(&format!("{session}.py"));
    std::fs::write(&path, program).expect("write program");
    env.pane_running(
        session,
        &format!("printf '{COLOUR_PROBE}\\n'\npython3 '{}'", path.display()),
    )
}

#[test]
fn a_capture_keeps_every_grapheme_cluster_whole() {
    let mut env = Env::new(80, 24);
    let pane = pane_running_python(
        &mut env,
        "graph",
        "import sys\nsys.stdout.write('e\\u0301 \\U0001f6e0\\ufe0f \\U0001f44d\\U0001f3fd \
         \\U0001f468\\u200d\\U0001f4bb \\U0001f1fa\\U0001f1f8 GRAPH' + 'EMES-DONE\\n')\n",
    );
    env.wait_for("graph", &pane, "GRAPHEMES-DONE");
    env.assert_colour_probe(&pane);
    let text = env.capture("graph", &pane);
    for cluster in ["e\u{0301}", "🛠\u{fe0f}", "👍🏽", "👨\u{200d}💻", "🇺🇸"] {
        assert!(
            text.contains(cluster),
            "capture split or lost {cluster:?}:\n{text}"
        );
    }
}

/// The program asks for the cursor position, the synchronized-output mode and
/// the terminal's version, then reports what came back on its own stdin.
#[test]
fn terminal_queries_get_their_replies_on_the_programs_stdin() {
    let mut env = Env::new(80, 24);
    let pane = pane_running_python(
        &mut env,
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
    env.wait_for("query", &pane, "QUERIES-DONE");
    env.assert_colour_probe(&pane);
    let text = env.capture("query", &pane);
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
    let mut env = Env::new(80, 24);
    let (ready, release) = (env.work("sync.ready"), env.work("sync.release"));
    let pane = pane_running_python(
        &mut env,
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
    env.assert_colour_probe(&pane);
    std::fs::write(&release, b"go").expect("release the frame");
    assert!(
        !frozen.contains("NEW-FRAME"),
        "the pending frame leaked:\n{frozen}"
    );
    assert!(
        frozen.contains("OLD-FRAME"),
        "the frame before the sync was lost:\n{frozen}"
    );
    env.wait_for("sync", &pane, "NEW-FRAME");
}
