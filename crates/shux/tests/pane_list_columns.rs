//! End-to-end: what `pane list` tells an operator about a pane (issue #135).
//!
//! `crates/shux/src/style.rs` unit-tests the rendering in isolation. This suite
//! drives the shipped `shux` binary — a real daemon in an isolated
//! `XDG_RUNTIME_DIR`, real PTYs, real shells — because the defect lived in the
//! gap between what `pane.list` returns and what the human formats print, and
//! that gap is only visible end to end. The text arm additionally needs a real
//! TTY (`TerminalContext::detect` downgrades Text to Plain when stdout is a
//! pipe), so it is driven from inside a real pane and read back through
//! `pane capture`.
//!
//! Every capture here carries a colour probe (truecolor + 256-indexed + basic
//! ANSI), so a monochrome or `NO_COLOR` regression cannot pass a run that only
//! ever compared characters.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

/// Truecolor + 256-indexed + basic ANSI on one line. Mandatory on every
/// daemon-backed capture (CLAUDE.md).
const COLOUR_PROBE: &str = "\\033[38;2;120;220;180mTRUECOLOR\\033[0m \\033[38;5;208mINDEXED\\033[0m \\033[34mBASIC\\033[0m";

/// The marker [`Env::run_line_and_wait`] waits for.
const DONE_MARKER: &str = "RENDER-DONE";

/// Build the line to type so the shell prints [`DONE_MARKER`] once `line` has
/// finished writing.
///
/// The marker's spelling is SPLIT across a quote boundary: the shell
/// concatenates the two adjacent quoted strings when it runs the line, but the
/// line the terminal ECHOES never contains the marker. Spell it whole here and
/// every caller silently returns on the echo instead of on the output —
/// `the_completion_marker_cannot_be_satisfied_by_the_echoed_line` pins that.
fn line_with_completion_marker(line: &str) -> String {
    format!("{line}; printf 'RENDER''-DONE\\n'")
}

/// Keeps a pane's screen alive after the interesting part has run.
const PARK: &str = "; exec sleep 900";

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

    fn work(&self) -> PathBuf {
        self.root.path().join("work")
    }

    fn shux(&self) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.env("XDG_RUNTIME_DIR", self.root.path().join("runtime"))
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env_remove("SHUX_SOCKET")
            .env("NO_COLOR", "1")
            // Pinned, not inherited: a `--cmd` pane is wrapped in `$SHELL -c`,
            // and the test asserts on the argv that produces.
            .env("SHELL", "/bin/sh")
            .current_dir(self.work());
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

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let mut full = vec!["--format", "json"];
        full.extend_from_slice(args);
        let out = self.ok(&full);
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("not json ({e}): {out}"))
    }

    #[track_caller]
    fn session(&mut self, name: &str, script: &str) -> String {
        let full = format!("{script}{PARK}");
        self.ok(&["session", "create", name, "-d", "--cmd", &full]);
        self.sessions.push(name.to_string());
        name.to_string()
    }

    /// A session whose pane is a live interactive shell.
    ///
    /// `pane.run_command` WRITES ITS LINE INTO THE PANE'S STDIN and waits for a
    /// marker to come back — so it only means anything when something in the
    /// pane is reading and executing lines. The `--cmd … exec sleep 900` panes
    /// the rest of this file uses have replaced their shell, and the line just
    /// sits on the screen unexecuted. That is not a defect; it is why these
    /// tests need a different fixture.
    #[track_caller]
    fn shell_session(&mut self, name: &str) -> String {
        self.ok(&["session", "create", name, "-d"]);
        self.sessions.push(name.to_string());
        // Prove the shell is live AND colour-probe the pane, in one line.
        let pane = self.json(&["pane", "list", "-s", name])[0]["id"]
            .as_str()
            .expect("pane id")
            .to_string();
        self.run_line_and_wait(name, "1", &format!("printf '{COLOUR_PROBE}\\n'"));
        pane
    }

    /// Type `line` into a pane and press Enter. `send-keys` sends bytes
    /// verbatim, so the newline has to travel as base64.
    fn type_line(&self, session: &str, window: &str, line: &str) {
        use base64::Engine as _;
        let data = base64::engine::general_purpose::STANDARD.encode(format!("{line}\n"));
        self.ok(&[
            "pane",
            "send-keys",
            "-s",
            session,
            "-w",
            window,
            "--data",
            &data,
        ]);
    }

    fn capture(&self, session: &str, window: &str) -> String {
        self.ok(&["pane", "capture", "-s", session, "-w", window])
    }

    /// Wait for `needle` to appear on a pane's screen. A `wait-settled` alone
    /// races a slow starter and captures a blank screen.
    #[track_caller]
    fn wait_for(&self, session: &str, window: &str, needle: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut last = String::new();
        while Instant::now() < deadline {
            last = self.capture(session, window);
            if last.contains(needle) {
                return last;
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        panic!("{needle:?} never appeared in {session}:{window}:\n{last}");
    }

    /// Type `line` into a pane and wait until the command has FINISHED writing.
    ///
    /// Waiting on a needle taken from the command's own output races the
    /// render. `pane list --format text` reaches the screen header-first, and
    /// `wait_for` happily returns that partial frame: CI caught this under
    /// llvm-cov, whose slower binary widens the split, with a captured screen
    /// holding `ID TITLE CWD COMMAND` and not one row beneath it.
    ///
    /// A needle that also occurs in the typed line is worse: the shell ECHOES
    /// the line, so the wait is satisfied before the command has run at all —
    /// the #167 failure exactly, green while testing nothing.
    ///
    /// So wait on a marker the shell prints only once the command has exited.
    /// Its spelling is split so that the echoed line cannot contain it, and
    /// both halves reach the same fd in order, so the marker cannot outrun the
    /// output it terminates.
    #[track_caller]
    fn run_line_and_wait(&self, session: &str, window: &str, line: &str) -> String {
        self.type_line(session, window, &line_with_completion_marker(line));
        self.wait_for(session, window, DONE_MARKER)
    }

    fn bin_str(&self) -> String {
        self.bin.to_string_lossy().to_string()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        for s in self.sessions.clone() {
            let _ = self.run(&["session", "kill", "-s", &s]);
        }
        let _ = self.run(&["daemon", "stop"]);
    }
}

/// Read the plain arm into (id, cwd, command, title) tuples.
fn plain_rows(out: &str) -> Vec<Vec<String>> {
    out.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.split('\t').map(String::from).collect())
        .collect()
}

// ── the issue ───────────────────────────────────────────────────────────

/// Defect 1: the pane's title — what its border draws and what an operator
/// calls it — reached `--format json` and neither human format.
#[test]
fn the_human_formats_name_every_pane() {
    let mut env = Env::new();
    let s = env.session("t135a", &format!("printf '{COLOUR_PROBE}\\n'"));
    // A second pane whose title differs from its program, so a test that
    // accidentally reads the command column cannot pass.
    env.ok(&["pane", "split", "-s", &s, "--", "sleep", "900"]);
    env.ok(&["pane", "title", "-s", &s, "-t", "the-operators-name"]);

    let json = env.json(&["pane", "list", "-s", &s]);
    let titles: Vec<String> = json
        .as_array()
        .expect("array")
        .iter()
        .map(|p| p["title"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        titles.iter().any(|t| t == "the-operators-name"),
        "set-up failed, no such title in {titles:?}"
    );

    let plain = env.ok(&["--format", "plain", "pane", "list", "-s", &s]);
    let rows = plain_rows(&plain);
    assert_eq!(rows.len(), titles.len(), "row count:\n{plain}");
    for row in &rows {
        assert_eq!(row.len(), 4, "plain arm must have 4 columns: {row:?}");
    }
    let plain_titles: Vec<&str> = rows.iter().map(|r| r[3].as_str()).collect();
    for t in &titles {
        assert!(
            plain_titles.contains(&t.as_str()),
            "title {t:?} is in json and not in plain:\n{plain}"
        );
    }
}

/// Defect 2, the issue's own reproduction: argv joined with a bare space, so a
/// quoted argument and several arguments print identically.
#[test]
fn one_argument_containing_a_space_is_not_printed_as_several() {
    let mut env = Env::new();
    let s = env.session("t135b", &format!("printf '{COLOUR_PROBE}\\n'"));
    // argv[3] is one argument with spaces in it; the pane really runs it.
    env.ok(&[
        "pane",
        "split",
        "-s",
        &s,
        "--",
        "/bin/sh",
        "-c",
        "sleep 900",
        "one two three",
    ]);

    let json = env.json(&["pane", "list", "-s", &s]);
    let argvs: Vec<Vec<String>> = json
        .as_array()
        .expect("array")
        .iter()
        .map(|p| {
            p["command"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|x| x.as_str().unwrap_or_default().to_string())
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect();
    let split_argv = argvs
        .iter()
        .find(|a| a.iter().any(|x| x == "one two three"))
        .unwrap_or_else(|| panic!("set-up failed: {argvs:?}"));
    assert_eq!(split_argv.len(), 4, "{split_argv:?}");

    let plain = env.ok(&["--format", "plain", "pane", "list", "-s", &s]);
    let commands: Vec<String> = plain_rows(&plain).iter().map(|r| r[2].clone()).collect();
    let rendered = commands
        .iter()
        .find(|c| c.contains("one two three"))
        .unwrap_or_else(|| panic!("argv missing from plain output:\n{plain}"));
    assert!(
        rendered.contains("'one two three'"),
        "the argument boundary is not recoverable: {rendered:?}"
    );

    // The real test of "recoverable": hand the rendered line back to a shell
    // and count the words. This is what the output claims to be.
    let split = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(format!(
            "for a in {rendered}; do printf '%s\\0' \"$a\"; done"
        ))
        .output()
        .expect("sh");
    let mut words: Vec<String> = String::from_utf8_lossy(&split.stdout)
        .split('\0')
        .map(String::from)
        .collect();
    words.pop();
    assert_eq!(
        words, *split_argv,
        "the printed line does not re-split into the argv it came from"
    );
}

/// The shell-wrapped shape #125 made normal, which is the case the issue was
/// filed about: `--cmd` produces `["<shell>", "-c", "<script>"]` and the script
/// is full of spaces and metacharacters.
#[test]
fn a_shell_wrapped_cmd_pane_prints_its_script_as_one_argument() {
    let mut env = Env::new();
    let s = env.session(
        "t135c",
        &format!("printf '{COLOUR_PROBE}\\n'; printf 'hi\\n'"),
    );
    let plain = env.ok(&["--format", "plain", "pane", "list", "-s", &s]);
    let row = &plain_rows(&plain)[0];
    assert!(
        row[2].starts_with("/bin/sh -c '"),
        "the script is not shown as one argument: {:?}",
        row[2]
    );
    assert!(row[2].ends_with('\''), "unterminated quote: {:?}", row[2]);
}

/// The completion marker must not survive into the line the shell ECHOES.
///
/// `run_line_and_wait` exists because a needle occurring in the typed line is
/// satisfied by the terminal's echo before the command has run at all — #167's
/// failure, stable and green while testing nothing. That safety rests entirely
/// on the marker being split across a quote boundary, so pin BOTH halves of the
/// property by execution rather than by eye: the echoed line must not contain
/// it, and a real shell must still emit it.
#[test]
fn the_completion_marker_cannot_be_satisfied_by_the_echoed_line() {
    let typed = line_with_completion_marker("shux pane list -s s -w 0 --format text");
    assert!(
        !typed.contains(DONE_MARKER),
        "the echoed line contains {DONE_MARKER:?} verbatim, so the wait would \
         return on the echo instead of the output: {typed}"
    );

    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(line_with_completion_marker("true"))
        .output()
        .expect("spawn sh");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        DONE_MARKER,
        "the shell no longer reassembles the marker, so the wait would time out"
    );
}

// ── cross-path consistency ──────────────────────────────────────────────

/// The three formats must agree about the same panes. They are three renderers
/// over one RPC response, and nothing but a test makes them stay that way.
#[test]
fn text_plain_and_json_agree_about_every_pane() {
    let mut env = Env::new();
    let s = env.session("t135d", &format!("printf '{COLOUR_PROBE}\\n'"));
    env.ok(&["pane", "split", "-s", &s, "--", "sleep", "900"]);
    // A window to read the text arm from, since text needs a real TTY.
    env.ok(&["window", "create", "-s", &s, "-n", "viewer"]);

    let json = env.json(&["pane", "list", "-s", &s, "-w", "0"]);
    let panes = json.as_array().expect("array");
    assert_eq!(panes.len(), 2, "set-up: {json}");

    let plain = env.ok(&["--format", "plain", "pane", "list", "-s", &s, "-w", "0"]);
    let rows = plain_rows(&plain);
    assert_eq!(rows.len(), panes.len());

    // Give the viewer a wide pane so nothing is truncated, then run the text
    // arm inside it.
    let viewer_pane = env.json(&["pane", "list", "-s", &s, "-w", "viewer"])[0]["id"]
        .as_str()
        .expect("viewer pane id")
        .to_string();
    env.ok(&[
        "pane",
        "set-size",
        "-s",
        &s,
        "-w",
        "viewer",
        "-p",
        &viewer_pane,
        "--cols",
        "200",
        "--rows",
        "24",
    ]);
    let cmd = format!("{} pane list -s {s} -w 0 --format text", env.bin_str());
    let screen = env.run_line_and_wait(&s, "viewer", &cmd);

    for pane in panes {
        let short = &pane["id"].as_str().expect("id")[..8];
        let title = pane["title"].as_str().expect("title");
        let cwd = pane["cwd"].as_str().expect("cwd");

        let row = rows
            .iter()
            .find(|r| r[0] == short)
            .unwrap_or_else(|| panic!("plain lost pane {short}:\n{plain}"));
        assert_eq!(row[1], cwd, "plain/json disagree on cwd");
        assert_eq!(row[3], title, "plain/json disagree on title");

        assert!(screen.contains(short), "text lost pane {short}:\n{screen}");
        assert!(
            screen.contains(title),
            "text lost the title {title:?} of pane {short}:\n{screen}"
        );
        // The command column is the one the issue is about; the argv's first
        // word must be visible in every format.
        let program = pane["command"][0].as_str().unwrap_or_default();
        if !program.is_empty() {
            assert!(
                row[2].contains(program),
                "plain lost the program {program:?}: {row:?}"
            );
        }
    }
}

/// Colour probe: the pane whose output the list describes is still rendering
/// all three colour classes, so a monochrome regression cannot pass this file.
#[test]
fn the_listed_pane_still_renders_every_colour_class() {
    let mut env = Env::new();
    let s = env.session("t135e", &format!("printf '{COLOUR_PROBE}\\n'"));
    env.wait_for(&s, "1", "TRUECOLOR");
    let pane = env.json(&["pane", "list", "-s", &s])[0]["id"]
        .as_str()
        .expect("pane id")
        .to_string();
    let cells = env.json(&["pane", "glance", &pane, "--cells", "--text-only"]);
    let text = cells.to_string();
    for needle in ["TRUECOLOR", "INDEXED", "BASIC"] {
        assert!(text.contains(needle), "colour probe missing {needle}");
    }

    // Assert on the PEN, structurally. A substring search cannot do this job:
    // `"4"` occurs in the row and column indices of any non-empty frame, so the
    // BASIC half of this test passed on a monochrome screen until the shux-tui-qa
    // gate for task 095 pointed it out. Walk the runs and collect the actual
    // foreground pens instead.
    let mut indexed: Vec<u64> = Vec::new();
    let mut truecolour = false;
    let mut visit = |style: &serde_json::Value| {
        let Some(fg) = style.get("fg") else { return };
        if let Some(idx) = fg.get("idx").and_then(|v| v.as_u64()) {
            indexed.push(idx);
        }
        if fg.get("rgb").is_some() {
            truecolour = true;
        }
    };
    for row in cells["result"]["cells"]["rows"]
        .as_array()
        .into_iter()
        .flatten()
    {
        for run in row["runs"].as_array().into_iter().flatten() {
            // A run is [col, content] or [col, content, style].
            if let Some(style) = run.as_array().and_then(|r| r.get(2)) {
                visit(style);
            }
        }
    }
    assert!(
        truecolour,
        "no run carries an rgb pen; the screen is monochrome:\n{text}"
    );
    for (word, idx) in [("INDEXED", 208u64), ("BASIC", 4u64)] {
        assert!(
            indexed.contains(&idx),
            "no run carries indexed pen {idx} for {word}; pens seen: {indexed:?}\n{text}"
        );
    }
}

// ── the sixth spawner: `pane.run_command`'s `args` ──────────────────────

/// `pane.run_command` read `args` with `filter_map(as_str)` — the exact silent
/// drop issue #125 removed from the five spawning RPCs, still live on the
/// sixth. `["a", null, "b"]` ran `a b` and reported success.
#[test]
fn run_command_rejects_a_non_string_argument_instead_of_dropping_it() {
    let mut env = Env::new();
    let s = "t135f".to_string();
    let pane = env.shell_session(&s);
    let params = serde_json::json!({
        "pane_id": pane,
        "command": "printf",
        "args": ["[%s]", "A", null, "B"],
        "timeout": 5,
    })
    .to_string();
    let out = env.run(&["rpc", "call", "pane.run_command", "--params", &params]);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("args[2]"),
        "a null argument was not named in the error: {combined}"
    );
    assert!(
        !env.capture(&s, "1").contains("[A][B]"),
        "the null was dropped and the command ran anyway"
    );
}

/// An argument full of shell metacharacters is an argument, not shell text —
/// `command` is where shell text goes. Asserted on the *state* as well as the
/// output: a `(` used to produce a syntax error that swallowed the completion
/// marker, so the call sat out its whole timeout.
#[test]
fn run_command_delivers_a_metacharacter_argument_whole_and_completes() {
    let mut env = Env::new();
    let s = "t135g".to_string();
    let pane = env.shell_session(&s);

    for arg in ["A;id", "A(B)", "=nosuchprog", "A|B", "{a,b}"] {
        let params = serde_json::json!({
            "pane_id": pane,
            "command": "printf",
            "args": ["[%s]", arg],
            "timeout": 10,
        })
        .to_string();
        let out = env.ok(&[
            "--format",
            "json",
            "rpc",
            "call",
            "pane.run_command",
            "--params",
            &params,
        ]);
        let v: serde_json::Value =
            serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
        assert_eq!(
            v["result"]["state"], "completed",
            "argument {arg:?} did not complete — the line the shell got was not parseable: {out}"
        );
        let screen = env.wait_for(&s, "1", &format!("[{arg}]"));
        assert!(
            !screen.contains("uid="),
            "argument {arg:?} executed `id`:\n{screen}"
        );
    }
}

/// An empty argument used to vanish from the line entirely.
#[test]
fn run_command_keeps_an_empty_argument() {
    let mut env = Env::new();
    let s = "t135h".to_string();
    let pane = env.shell_session(&s);
    let params = serde_json::json!({
        "pane_id": pane,
        "command": "printf",
        "args": ["<%s>", "A", "", "B"],
        "timeout": 10,
    })
    .to_string();
    env.ok(&["rpc", "call", "pane.run_command", "--params", &params]);
    let screen = env.wait_for(&s, "1", "<A><><B>");
    assert!(screen.contains("<A><><B>"), "{screen}");
}

/// A control byte in `args` used to leave the pane at its continuation prompt
/// FOREVER, swallowing every later command.
///
/// `args` is shell-quoted and then TYPED INTO THE TERMINAL, so the tty line
/// discipline reads `0x03` as INTR and truncates the line — inside the single
/// quotes the quoting just added. Bash drops to `>` and never comes back.
/// Correct quoting is what made this permanent rather than transient, so the
/// validation has to come with it.
#[test]
fn a_control_byte_in_args_is_refused_and_leaves_the_pane_usable() {
    let mut env = Env::new();
    let s = "t135i".to_string();
    let pane = env.shell_session(&s);

    for byte in ['\u{3}', '\u{15}', '\u{1a}', '\u{7f}'] {
        let params = serde_json::json!({
            "pane_id": pane,
            "command": "printf",
            "args": ["[%s]", format!("a{byte}b")],
            "timeout": 5,
        })
        .to_string();
        let out = env.run(&["rpc", "call", "pane.run_command", "--params", &params]);
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            combined.contains("args[1]") && combined.contains("control character"),
            "U+{:04X} was accepted: {combined}",
            byte as u32
        );
    }

    // The pane must still work. This is the half that actually failed: the
    // rejected call never reached the PTY, so nothing was truncated.
    let params = serde_json::json!({
        "pane_id": pane,
        "command": "printf",
        "args": ["[%s]", "STILL-ALIVE"],
        "timeout": 15,
    })
    .to_string();
    let out = env.ok(&[
        "--format",
        "json",
        "rpc",
        "call",
        "pane.run_command",
        "--params",
        &params,
    ]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
    assert_eq!(
        v["result"]["state"], "completed",
        "the pane was wedged by a rejected call: {out}"
    );
    env.wait_for(&s, "1", "[STILL-ALIVE]");
}

/// The box frame must be square on the RENDERED GRID, whatever the title holds.
///
/// This asserts on `pane glance --cells` — the columns shux-vt actually painted
/// — and never on `style.rs::display_width`. That distinction is the whole
/// point of the test. The width tests in `style.rs` measure with the same
/// function they are validating, so they are self-referential with respect to
/// the real cell allocator and cannot see a disagreement between the two; the
/// `shux-tui-qa` gate for task 095 found exactly such a disagreement that the
/// entire unit suite was blind to.
///
/// Ignored until #144 lands. shux-vt gives an emoji-presentation sequence
/// (`⚠️` = U+26A0 U+FE0F) ONE cell while `UnicodeWidthStr` — and xterm, iTerm2
/// and wezterm — give it two, so a title carrying one shifts its row a column
/// left and the right border lands short. The fix belongs in shux-vt's cell
/// allocation, not in this listing's padding: matching `style.rs` to shux-vt
/// would square the frame inside a shux pane and break it in every other
/// terminal. Un-ignore when #144 is fixed.
#[test]
#[ignore = "blocked on #144: shux-vt allocates 1 cell for VS16 emoji, real terminals allocate 2"]
fn the_box_frame_is_square_on_the_rendered_grid() {
    let mut env = Env::new();
    let s = "t135f";
    // `shell_session` returns the live shell's PANE id — it lists itself below.
    let target = env.shell_session(s);

    // Title it with an emoji-presentation sequence. The header and border rows
    // are the control: they are pure ASCII, so a row that disagrees with them
    // disagrees because of its title.
    env.ok(&["pane", "title", "-s", s, "-p", &target, "-t", "⚠️ build"]);

    env.ok(&[
        "pane", "set-size", "-s", s, "-p", &target, "--cols", "100", "--rows", "20",
    ]);
    env.run_line_and_wait(
        s,
        "1",
        &format!("clear; {} --format text pane list -s {s}", env.bin_str()),
    );

    let cells = env.json(&["pane", "glance", &target, "--cells"]);
    let rows = cells["result"]["cells"]["rows"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    // Right-border column of every framed row, read off the grid.
    let mut borders: Vec<u64> = Vec::new();
    for row in &rows {
        let mut last: Option<u64> = None;
        for run in row["runs"].as_array().into_iter().flatten() {
            let (Some(col), Some(content)) = (
                run.get(0).and_then(|v| v.as_u64()),
                run.get(1).and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            if let Some(off) = content.rfind('│') {
                last = Some(col + content[..off].chars().count() as u64);
            }
        }
        if let Some(c) = last {
            borders.push(c);
        }
    }

    assert!(
        borders.len() >= 3,
        "expected a framed listing, got borders {borders:?}"
    );
    let first = borders[0];
    assert!(
        borders.iter().all(|c| *c == first),
        "the frame is ragged: right borders landed at {borders:?} — every framed \
         row must end in the same column regardless of what its title contains"
    );
}
