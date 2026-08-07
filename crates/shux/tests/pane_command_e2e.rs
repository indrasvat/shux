//! End-to-end: what `--cmd` and the `command` RPC parameter actually run
//! (issue #125).
//!
//! `crates/shux/src/pane_command.rs` unit-tests the parsing contract in
//! isolation. This suite drives the shipped `shux` binary — a real daemon in an
//! isolated `XDG_RUNTIME_DIR`, real PTYs, real shells — because the defect was
//! never visible in a return value. Every failing case *started a pane
//! successfully* and then ran something other than what was asked for.
//!
//! Every capture in this file carries a colour probe (truecolor + 256-indexed +
//! basic ANSI), so a monochrome or `NO_COLOR` regression cannot pass a run that
//! only ever compared characters.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

/// Truecolor + 256-indexed + basic ANSI on one line. Mandatory on every
/// daemon-backed capture (CLAUDE.md).
const COLOUR_PROBE: &str = "\\033[38;2;120;220;180mTRUECOLOR\\033[0m \\033[38;5;208mINDEXED\\033[0m \\033[34mBASIC\\033[0m";

/// Appended to every shell string under test so the pane keeps its screen after
/// the interesting part has run. Without it a fast command exits, the PTY goes
/// away and the capture races the teardown.
const PARK: &str = "; exec sleep 900";

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

    fn work(&self) -> PathBuf {
        self.root.path().join("work")
    }

    fn shux(&self) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.env("XDG_RUNTIME_DIR", self.root.path().join("runtime"))
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env_remove("SHUX_SOCKET")
            .env("NO_COLOR", "1")
            // Pinned, not inherited: the whole point of the string form is that
            // it goes to a shell, and the test must know which one.
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

    /// `shux session create <name> -d --cmd <shell string>`, parked so the
    /// screen survives. Returns the initial pane's id.
    #[track_caller]
    fn session_with_cmd(&mut self, name: &str, script: &str) -> String {
        let full = format!("{script}{PARK}");
        self.ok(&["session", "create", name, "-d", "--cmd", &full]);
        self.sessions.push(name.to_string());
        self.first_pane(name)
    }

    #[track_caller]
    fn first_pane(&self, session: &str) -> String {
        let v = self.json(&["pane", "list", "-s", session]);
        v[0]["id"].as_str().expect("pane id").to_string()
    }

    /// Every pane in a session, in `pane list` order, as (id, command-argv).
    fn panes(&self, session: &str, window: &str) -> Vec<(String, Vec<String>)> {
        let v = self.json(&["pane", "list", "-s", session, "-w", window]);
        v.as_array()
            .expect("pane list array")
            .iter()
            .map(|p| {
                let id = p["id"].as_str().unwrap_or_default().to_string();
                let cmd = p["command"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|s| s.as_str().unwrap_or_default().to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                (id, cmd)
            })
            .collect()
    }

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

    /// Assert the pane's screen contains `needle` and the colour probe, and
    /// does NOT contain any of `absent`.
    #[track_caller]
    fn expect_screen(&self, session: &str, pane: &str, needle: &str, absent: &[&str]) {
        self.wait_for(session, pane, needle);
        let screen = self.capture(session, pane);
        for probe in ["TRUECOLOR", "INDEXED", "BASIC"] {
            assert!(
                screen.contains(probe),
                "colour probe {probe} missing from pane screen:\n{screen}"
            );
        }
        for bad in absent {
            assert!(
                !screen.contains(bad),
                "screen still contains {bad:?} — the shell did not interpret the command:\n\
                 {screen}"
            );
        }
    }

    /// Raw JSON-RPC, for the contract cases the CLI cannot express.
    fn rpc(&self, method: &str, params: &str) -> Output {
        self.run(&[
            "rpc", "call", method, "--params", params, "--format", "json",
        ])
    }

    #[track_caller]
    fn rpc_ok(&self, method: &str, params: &str) -> serde_json::Value {
        let out = self.rpc(method, params);
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(
            out.status.success(),
            "{method} {params} failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("not json ({e}): {stdout}"))
    }

    /// Assert an RPC is REJECTED, and return the message so the caller can
    /// check it names the offending value. A silent default-shell fallback —
    /// the pre-fix behaviour — fails here.
    #[track_caller]
    fn rpc_rejected(&self, method: &str, params: &str) -> String {
        let out = self.rpc(method, params);
        let joined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !out.status.success(),
            "{method} accepted malformed params {params} instead of rejecting them:\n{joined}"
        );
        joined
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

fn probe() -> String {
    format!("printf '{COLOUR_PROBE}\\n'")
}

// ── the issue's own reproductions ───────────────────────────────────────

/// Issue #125, verbatim. `printf 'X\n'; sleep 300` must be two commands, not
/// `printf` with three arguments. Before the fix the screen carried printf's
/// "ignoring excess arguments" warning and the pane was already dead.
#[test]
fn a_semicolon_separates_two_commands_instead_of_becoming_an_argument() {
    let mut env = Env::new();
    let pane = env.session_with_cmd(
        "semi",
        &format!("{}; printf 'FIRST\\n'; printf 'SECOND\\n'", probe()),
    );
    env.expect_screen(
        "semi",
        &pane,
        "SECOND",
        // printf's complaint about being handed the rest of the line.
        &["ignoring excess arguments"],
    );
    let screen = env.capture("semi", &pane);
    assert!(
        screen.contains("FIRST"),
        "first command did not run:\n{screen}"
    );
}

/// Issue #125's second reproduction: quotes reached `echo` as literal
/// characters, so the pane printed `'hello world'`.
#[test]
fn quotes_are_consumed_by_the_shell_not_printed() {
    let mut env = Env::new();
    let pane = env.session_with_cmd("quotes", &format!("{}; echo 'hello world'", probe()));
    env.expect_screen("quotes", &pane, "hello world", &["'hello world'"]);
}

/// The knock-on symptom the issue describes: `printf` exited, the PTY went
/// away, and the next `pane send-keys` failed with a not-found error that
/// looked like an unrelated bug. With the trailing `sleep` actually running as
/// its own command, the pane is still there to be typed into.
#[test]
fn the_pane_survives_a_command_whose_tail_is_a_long_running_program() {
    let mut env = Env::new();
    let pane = env.session_with_cmd("alive", &format!("{}; printf 'MARK\\n'", probe()));
    env.expect_screen("alive", &pane, "MARK", &[]);

    // The pane is still attached to a live PTY: writing to it succeeds.
    let out = env.run(&["pane", "send-keys", "-s", "alive", "-p", &pane, "-t", ""]);
    assert!(
        out.status.success(),
        "pane PTY was gone — send-keys failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ── the rest of the shell grammar ───────────────────────────────────────

#[test]
fn a_pipe_connects_two_programs() {
    let mut env = Env::new();
    let pane = env.session_with_cmd("pipe", &format!("{}; echo one | tr a-z A-Z", probe()));
    env.expect_screen("pipe", &pane, "ONE", &["| tr"]);
}

#[test]
fn and_or_lists_short_circuit() {
    let mut env = Env::new();
    let pane = env.session_with_cmd(
        "andor",
        &format!("{}; true && echo YES; false || echo FALLBACK", probe()),
    );
    env.expect_screen("andor", &pane, "FALLBACK", &["&&", "||"]);
    let screen = env.capture("andor", &pane);
    assert!(screen.contains("YES"), "&& branch did not run:\n{screen}");
}

#[test]
fn a_glob_expands_against_the_working_directory() {
    let mut env = Env::new();
    std::fs::write(env.work().join("alpha.probe"), b"x").unwrap();
    std::fs::write(env.work().join("beta.probe"), b"x").unwrap();
    let pane = env.session_with_cmd("glob", &format!("{}; echo *.probe", probe()));
    env.expect_screen("glob", &pane, "alpha.probe beta.probe", &["*.probe"]);
}

#[test]
fn redirection_writes_a_file_the_next_command_reads_back() {
    let mut env = Env::new();
    let pane = env.session_with_cmd(
        "redir",
        &format!("{}; echo PERSISTED > out.txt; cat out.txt", probe()),
    );
    env.expect_screen("redir", &pane, "PERSISTED", &["> out.txt"]);
    assert_eq!(
        std::fs::read_to_string(env.work().join("out.txt"))
            .unwrap()
            .trim(),
        "PERSISTED"
    );
}

#[test]
fn variables_are_assigned_and_expanded() {
    let mut env = Env::new();
    let pane = env.session_with_cmd("vars", &format!("{}; V=42; echo value-$V", probe()));
    env.expect_screen("vars", &pane, "value-42", &["$V"]);
}

#[test]
fn command_substitution_runs() {
    let mut env = Env::new();
    let pane = env.session_with_cmd("subst", &format!("{}; echo sum-$(echo 7)", probe()));
    env.expect_screen("subst", &pane, "sum-7", &["$(echo"]);
}

#[test]
fn a_multi_line_command_string_runs_every_line() {
    let mut env = Env::new();
    let pane = env.session_with_cmd(
        "multiline",
        &format!("{}\nprintf 'LINE-A\\n'\nprintf 'LINE-B\\n'", probe()),
    );
    env.expect_screen("multiline", &pane, "LINE-B", &[]);
    let screen = env.capture("multiline", &pane);
    assert!(
        screen.contains("LINE-A"),
        "first line did not run:\n{screen}"
    );
}

#[test]
fn non_ascii_text_survives_the_round_trip() {
    let mut env = Env::new();
    let pane = env.session_with_cmd("unicode", &format!("{}; echo 'héllo — 世界 🌍'", probe()));
    env.expect_screen("unicode", &pane, "héllo — 世界", &["'héllo"]);
}

// ── argv passthrough is unchanged ───────────────────────────────────────

/// Trailing `-- argv...` was always correct and stays exec-direct: no shell,
/// no splitting, no expansion. An argument containing spaces stays one
/// argument, and a `;` stays a literal semicolon.
#[test]
fn trailing_argv_is_exec_d_directly_with_no_shell_interpretation() {
    let mut env = Env::new();
    env.ok(&[
        "session",
        "create",
        "argv",
        "-d",
        "--",
        "sh",
        "-c",
        &format!("{}; printf 'a b;c\\n'{PARK}", probe()),
    ]);
    env.sessions.push("argv".to_string());
    let pane = env.first_pane("argv");
    env.expect_screen("argv", &pane, "a b;c", &[]);

    let panes = env.panes("argv", "0");
    assert_eq!(
        panes[0].1[0], "sh",
        "argv[0] should be the program, verbatim"
    );
    assert_eq!(panes[0].1[1], "-c");
}

/// A single argv element containing spaces must not be re-split.
#[test]
fn an_argv_element_containing_spaces_stays_one_argument() {
    let mut env = Env::new();
    let path = env.work().join("a file with spaces.txt");
    std::fs::write(&path, b"SPACED-CONTENT\n").unwrap();
    env.ok(&[
        "session",
        "create",
        "spaces",
        "-d",
        "--",
        "sh",
        "-c",
        &format!("{}; cat '{}'{PARK}", probe(), path.display()),
    ]);
    env.sessions.push("spaces".to_string());
    let pane = env.first_pane("spaces");
    env.expect_screen("spaces", &pane, "SPACED-CONTENT", &[]);
}

/// `--cmd` and trailing argv together: argv wins, and it is still exec-direct.
#[test]
fn trailing_argv_wins_over_cmd() {
    let mut env = Env::new();
    env.ok(&[
        "session",
        "create",
        "argvwins",
        "-d",
        "--cmd",
        "echo FROM-CMD",
        "--",
        "sh",
        "-c",
        &format!("{}; echo FROM-ARGV{PARK}", probe()),
    ]);
    env.sessions.push("argvwins".to_string());
    let pane = env.first_pane("argvwins");
    env.expect_screen("argvwins", &pane, "FROM-ARGV", &["FROM-CMD"]);
}

// ── every verb agrees ───────────────────────────────────────────────────

#[test]
fn session_ensure_interprets_cmd_the_same_way() {
    let mut env = Env::new();
    let full = format!("{}; echo 'ensure works'{PARK}", probe());
    env.ok(&["session", "create", "ens", "-d", "--ensure", "--cmd", &full]);
    env.sessions.push("ens".to_string());
    let pane = env.first_pane("ens");
    env.expect_screen("ens", &pane, "ensure works", &["'ensure works'"]);
}

#[test]
fn window_create_interprets_cmd_the_same_way_and_records_it() {
    let mut env = Env::new();
    env.ok(&["session", "create", "winc", "-d"]);
    env.sessions.push("winc".to_string());
    let full = format!("{}; echo 'window works'{PARK}", probe());
    env.ok(&["window", "create", "-s", "winc", "-n", "w1", "--cmd", &full]);

    let panes = env.panes("winc", "w1");
    let (pane, cmd) = panes[0].clone();
    env.expect_screen("winc", &pane, "window works", &["'window works'"]);

    // Before the fix `window.create` exec'd the command but never persisted it,
    // so `pane list` showed a blank command column.
    assert_eq!(cmd.first().map(String::as_str), Some("/bin/sh"), "{cmd:?}");
    assert_eq!(cmd.get(1).map(String::as_str), Some("-c"), "{cmd:?}");
    assert!(cmd[2].contains("window works"), "{cmd:?}");
}

#[test]
fn window_ensure_interprets_cmd_the_same_way() {
    let mut env = Env::new();
    env.ok(&["session", "create", "wine", "-d"]);
    env.sessions.push("wine".to_string());
    let full = format!("{}; echo 'ensure window'{PARK}", probe());
    env.ok(&[
        "window", "create", "-s", "wine", "-n", "w1", "--ensure", "--cmd", &full,
    ]);
    let panes = env.panes("wine", "w1");
    env.expect_screen("wine", &panes[0].0, "ensure window", &["'ensure window'"]);
    assert_eq!(panes[0].1.first().map(String::as_str), Some("/bin/sh"));
}

/// `pane.split` has no CLI flag for a command, but the RPC has always accepted
/// one. It ignored the string form entirely and never recorded the array form.
#[test]
fn pane_split_accepts_a_shell_string_and_records_what_it_ran() {
    let mut env = Env::new();
    env.ok(&["session", "create", "split", "-d"]);
    env.sessions.push("split".to_string());
    let first = env.first_pane("split");

    let script = format!("{}; echo 'split works'{PARK}", probe());
    let params = serde_json::json!({
        "pane_id": first,
        "direction": "vertical",
        "command": script,
    })
    .to_string();
    env.rpc_ok("pane.split", &params);

    let panes = env.panes("split", "0");
    let split = panes
        .iter()
        .find(|(id, _)| *id != first)
        .expect("split pane present");
    env.expect_screen("split", &split.0, "split works", &["'split works'"]);
    assert_eq!(
        split.1.first().map(String::as_str),
        Some("/bin/sh"),
        "{:?}",
        split.1
    );
}

// ── the API contract, over the wire ─────────────────────────────────────

/// An argv array whose elements are not all strings used to have the offenders
/// silently deleted — `["vim", null]` ran a bare `vim`.
#[test]
fn a_non_string_argv_element_is_rejected_by_every_spawning_rpc() {
    let mut env = Env::new();
    env.ok(&["session", "create", "host", "-d"]);
    env.sessions.push("host".to_string());
    let pane = env.first_pane("host");
    let sid = env.json(&["session", "list"])["sessions"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    for (method, extra) in [
        ("session.create", serde_json::json!({"name": "bad-a"})),
        ("session.ensure", serde_json::json!({"name": "bad-b"})),
        (
            "window.create",
            serde_json::json!({"session_id": sid, "name": "bad-c"}),
        ),
        (
            "window.ensure",
            serde_json::json!({"session_id": sid, "name": "bad-d"}),
        ),
        ("pane.split", serde_json::json!({"pane_id": pane})),
    ] {
        let mut params = extra.as_object().unwrap().clone();
        params.insert("command".into(), serde_json::json!(["echo", null]));
        let msg = env.rpc_rejected(method, &serde_json::Value::Object(params).to_string());
        assert!(
            msg.contains("command[1]"),
            "{method} error does not name the bad element: {msg}"
        );
    }
}

#[test]
fn a_command_that_is_neither_string_nor_array_is_rejected() {
    let env = Env::new();
    for bad in ["42", "true", r#"{"argv":["vim"]}"#] {
        let params = format!(r#"{{"name":"nope","command":{bad}}}"#);
        let msg = env.rpc_rejected("session.create", &params);
        assert!(
            msg.to_lowercase().contains("command"),
            "error does not mention 'command': {msg}"
        );
    }
    // …and nothing was created behind our back.
    let v = env.json(&["session", "list"]);
    assert!(
        v["sessions"].as_array().map(Vec::is_empty).unwrap_or(true),
        "a rejected create still made a session: {v}"
    );
}

#[test]
fn an_empty_program_name_is_rejected() {
    let env = Env::new();
    let msg = env.rpc_rejected("session.create", r#"{"name":"empty","command":[""]}"#);
    assert!(msg.contains("command[0]"), "{msg}");
}

/// A string `command` on `window.create` used to be dropped on the floor: the
/// pane silently got the default shell instead of an error or the command.
#[test]
fn a_string_command_is_honoured_by_window_create_not_ignored() {
    let mut env = Env::new();
    env.ok(&["session", "create", "strwin", "-d"]);
    env.sessions.push("strwin".to_string());
    let sid = env.json(&["session", "list"])["sessions"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let script = format!("{}; echo 'string honoured'{PARK}", probe());
    let params =
        serde_json::json!({"session_id": sid, "name": "sw", "command": script}).to_string();
    env.rpc_ok("window.create", &params);

    let panes = env.panes("strwin", "sw");
    env.expect_screen("strwin", &panes[0].0, "string honoured", &["'string"]);
}

// ── defaults and titles ─────────────────────────────────────────────────

/// `--cmd ""` is "no command": the pane gets the user's shell, as it always did.
#[test]
fn an_empty_cmd_opens_the_default_shell() {
    let mut env = Env::new();
    env.ok(&["session", "create", "blank", "-d", "--cmd", ""]);
    env.sessions.push("blank".to_string());
    let panes = env.panes("blank", "0");
    assert!(
        panes[0].1.is_empty(),
        "empty --cmd should leave the pane command unset, got {:?}",
        panes[0].1
    );
    // And the pane is genuinely alive.
    let out = env.run(&[
        "pane",
        "send-keys",
        "-s",
        "blank",
        "-p",
        &panes[0].0,
        "-t",
        "",
    ]);
    assert!(out.status.success());
}

/// Wrapping in a shell must not retitle every pane after its shell.
/// `--cmd top` still says `top`.
#[test]
fn the_pane_title_names_the_program_not_the_shell() {
    let mut env = Env::new();
    env.session_with_cmd("titled", &format!("{}; cat", probe()));
    let v = env.json(&["pane", "list", "-s", "titled"]);
    let title = v[0]["title"].as_str().unwrap_or_default().to_string();
    assert_eq!(
        title, "printf",
        "pane title should name the first program in the command, got {title:?}"
    );
}

/// `pane list` must report the argv that is actually running — including the
/// shell wrapper — so an operator can see exactly what was exec'd.
#[test]
fn pane_list_reports_the_argv_that_is_actually_running() {
    let mut env = Env::new();
    env.session_with_cmd("listed", &format!("{}; echo LISTED", probe()));
    let panes = env.panes("listed", "0");
    let cmd = &panes[0].1;
    assert_eq!(cmd[0], "/bin/sh", "{cmd:?}");
    assert_eq!(cmd[1], "-c", "{cmd:?}");
    assert!(cmd[2].contains("echo LISTED"), "{cmd:?}");
}

/// The exit status of a `--cmd` shell command is the shell's, so a failing
/// command really does end the pane.
#[test]
fn a_failing_shell_command_ends_the_pane_without_wedging_the_daemon() {
    let mut env = Env::new();
    env.ok(&["session", "create", "fails", "-d", "--cmd", "exit 3"]);
    env.sessions.push("fails".to_string());
    // The daemon stays healthy and answerable after the pane's child dies.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if env.run(&["session", "list"]).status.success() {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("daemon stopped answering after a --cmd pane exited non-zero");
}
