//! End-to-end: what a real pane's program writes, read back through the daemon.
//!
//! The VT behaviour itself is tested in `shux-vt`. These cover what only the
//! daemon's path can break: non-ASCII text through PTY, RPC and CLI, and
//! terminal replies written back to the program.

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
        &format!(
            r#"import os, sys, termios, tty
fd = 0
saved = termios.tcgetattr(fd)
tty.setraw(fd)
want = (b"\x1b[5;10R", b"\x1b[?2026;2$y", b"\x1bP>|shux {version}\x1b\\")
data = b""
try:
    os.write(1, b"\x1b[5;10H\x1b[6n\x1b[?2026$p\x1b[>0q")
    while not all(w in data for w in want):
        data += os.read(fd, 256)
finally:
    termios.tcsetattr(fd, termios.TCSADRAIN, saved)
sys.stdout.write("\r\nQUERIES-" + "DONE\r\n")
sys.stdout.flush()
"#,
            version = env!("CARGO_PKG_VERSION"),
        ),
    );
    // The program prints its marker only once all three replies (CPR,
    // DECRQM ?2026 and the whole XTVERSION string) have reached its stdin.
    env.wait_for("query", &pane, "QUERIES-DONE");
    env.assert_colour_probe(&pane);
}
