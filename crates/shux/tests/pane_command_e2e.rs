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

    /// Poll `pane list` until the pane running `command_needle` reports a
    /// numeric `exit_status`, then return the list as of that moment.
    ///
    /// Keyed on the COMMAND, not on "any pane with an exit status". A scenario
    /// with one pane that finishes and one that never starts has TWO panes that
    /// stop, and which records first is platform-dependent — a failed spawn can
    /// report immediately while a real process still has to run and be reaped.
    /// "First pane with an exit status" therefore picks a different pane on
    /// different machines, and the caller compares focus against the wrong one.
    #[track_caller]
    fn wait_for_exited_pane(&self, session: &str, command_needle: &str) -> serde_json::Value {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let panes = self.json(&["pane", "list", "-s", session]);
            let ready = panes.as_array().expect("panes").iter().any(|p| {
                p["exit_status"].is_number() && p["command"].to_string().contains(command_needle)
            });
            if ready {
                return panes;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the pane running {command_needle:?} never reported an exit_status \
                 within 20s; panes were:\n{panes:#}"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
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

// ── follow-ups found by adversarial review ──────────────────────────────
//
// Every case below was a live defect on the first cut of the fix (or a
// pre-existing one the fix made newly reachable), reproduced against the real
// binary before it was fixed.

/// `session.ensure` parsed `command` before its already-exists shortcut;
/// `window.ensure` parsed it after. So `window.ensure` accepted every
/// malformed shape without complaint — but only when the window happened to
/// exist, which is the case the verb is named for.
#[test]
fn window_ensure_validates_command_even_when_the_window_already_exists() {
    let mut env = Env::new();
    env.ok(&["session", "create", "ens", "-d"]);
    env.sessions.push("ens".to_string());
    let sid = env.json(&["session", "list"])["sessions"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    env.ok(&["window", "create", "-s", "ens", "-n", "already"]);

    for bad in ["42", "true", r#"{"a":1}"#, r#"["vim",null]"#, r#"[""]"#] {
        let params = format!(r#"{{"session_id":"{sid}","name":"already","command":{bad}}}"#);
        let msg = env.rpc_rejected("window.ensure", &params);
        assert!(msg.to_lowercase().contains("command"), "{bad}: {msg}");
    }
}

/// `[""]` was rejected but `["   "]` was not, and both exec a program name
/// that cannot resolve.
#[test]
fn a_blank_program_name_is_rejected_not_just_an_empty_one() {
    let env = Env::new();
    for bad in [r#"[""]"#, r#"["   "]"#, r#"["\t"]"#] {
        let msg = env.rpc_rejected(
            "session.create",
            &format!(r#"{{"name":"blank","command":{bad}}}"#),
        );
        assert!(msg.contains("command[0]"), "{bad}: {msg}");
    }
    let v = env.json(&["session", "list"]);
    assert!(v["sessions"].as_array().map(Vec::is_empty).unwrap_or(true));
}

/// An argument too long for `execve` was accepted and then failed silently.
///
/// Sent via `--params @FILE`, not inline: 128 KiB on a command line is itself
/// past `MAX_ARG_STRLEN`, so an inline attempt fails inside `shux`'s own exec
/// and never reaches the daemon.
#[test]
fn an_argument_too_long_for_execve_is_rejected_up_front() {
    let env = Env::new();
    let huge = "x".repeat(128 * 1024 + 1);
    let path = env.work().join("huge.json");
    std::fs::write(
        &path,
        serde_json::json!({"name": "big", "command": huge}).to_string(),
    )
    .unwrap();
    let out = env.run(&[
        "rpc",
        "call",
        "session.create",
        "--params",
        &format!("@{}", path.display()),
        "--format",
        "json",
    ]);
    let joined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "oversize command accepted:\n{joined}"
    );
    assert!(joined.contains("cannot exceed"), "{joined}");
}

/// The headline follow-up: a PTY that never started is not a pane. The RPC
/// returned success, the CLI printed "✓ Created session", and every later verb
/// answered "pane VT not found" against a phantom.
#[test]
fn a_program_that_cannot_be_executed_is_an_error_not_a_phantom_session() {
    let env = Env::new();
    let out = env.run(&[
        "session",
        "create",
        "ghost",
        "-d",
        "--",
        "no-such-binary-xyz",
    ]);
    let joined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "creating a session whose program does not exist reported success:\n{joined}"
    );
    assert!(
        joined.contains("No such file") || joined.to_lowercase().contains("spawn"),
        "error does not say what went wrong:\n{joined}"
    );
    // …and nothing was left behind.
    let v = env.json(&["session", "list"]);
    assert!(
        v["sessions"].as_array().map(Vec::is_empty).unwrap_or(true),
        "a failed create left a phantom session: {v}"
    );
}

/// Same contract for the other three spawning verbs.
#[test]
fn a_failed_spawn_leaves_no_phantom_window_or_pane() {
    let mut env = Env::new();
    env.ok(&["session", "create", "host", "-d"]);
    env.sessions.push("host".to_string());
    let sid = env.json(&["session", "list"])["sessions"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let pane = env.first_pane("host");

    let windows_before = env
        .json(&["window", "list", "-s", "host"])
        .as_array()
        .map(Vec::len)
        .unwrap_or(0);
    let panes_before = env.panes("host", "0").len();

    let bad = serde_json::json!(["no-such-binary-xyz"]);
    for (method, extra) in [
        (
            "window.create",
            serde_json::json!({"session_id": sid, "name": "ghostw"}),
        ),
        (
            "window.ensure",
            serde_json::json!({"session_id": sid, "name": "ghostw2"}),
        ),
        ("pane.split", serde_json::json!({"pane_id": pane})),
    ] {
        let mut params = extra.as_object().unwrap().clone();
        params.insert("command".into(), bad.clone());
        env.rpc_rejected(method, &serde_json::Value::Object(params).to_string());
    }

    assert_eq!(
        env.json(&["window", "list", "-s", "host"])
            .as_array()
            .map(Vec::len)
            .unwrap_or(0),
        windows_before,
        "a failed window spawn left a window behind"
    );
    assert_eq!(
        env.panes("host", "0").len(),
        panes_before,
        "a failed split left a pane behind"
    );
}

/// `state.apply` is a sixth way to give a pane a command, and it took its ops
/// as typed structs — so serde proved `Vec<String>` and nothing proved the
/// strings could reach `execve`. `[""]` committed a session, a window and a
/// pane whose PTY then failed to spawn.
#[test]
fn state_apply_rejects_an_argv_that_cannot_be_executed() {
    let env = Env::new();
    for bad in [r#"[""]"#, r#"["   "]"#] {
        let params = format!(
            r#"{{"ops":[{{"op":"create_session","name":"tpl","cwd":"/tmp","initial_command":{bad}}}]}}"#
        );
        let msg = env.rpc_rejected("state.apply", &params);
        assert!(msg.contains("initial_command"), "{bad}: {msg}");
    }
    let v = env.json(&["session", "list"]);
    assert!(
        v["sessions"].as_array().map(Vec::is_empty).unwrap_or(true),
        "a rejected apply still committed a session: {v}"
    );
}

/// `pane.split` has accepted a `command` since it was written; the CLI had no
/// way to say it, which broke "every subcommand is a thin JSON-RPC call".
#[test]
fn pane_split_cli_accepts_a_shell_command_and_trailing_argv() {
    let mut env = Env::new();
    env.ok(&["session", "create", "splitcli", "-d"]);
    env.sessions.push("splitcli".to_string());
    let first = env.first_pane("splitcli");

    env.ok(&[
        "pane",
        "split",
        "-s",
        "splitcli",
        "-p",
        &first,
        "-d",
        "vertical",
        "--cmd",
        &format!("{}; echo 'split by cmd'{PARK}", probe()),
    ]);
    let panes = env.panes("splitcli", "0");
    let split = panes
        .iter()
        .find(|(id, _)| *id != first)
        .expect("split pane present");
    env.expect_screen("splitcli", &split.0, "split by cmd", &["'split by cmd'"]);
    assert_eq!(
        split.1.first().map(String::as_str),
        Some("/bin/sh"),
        "{:?}",
        split.1
    );
}

/// A set-but-EMPTY `$SHELL` used to give a working `--cmd` pane and a DEAD
/// default pane, from the same daemon: the string form treated blank as unset,
/// `PtyConfig::resolve_command` did not, and `env::var` returns `Ok("")` rather
/// than an error. Both paths now agree.
#[test]
fn a_blank_shell_env_still_opens_a_working_default_pane() {
    let mut env = Env::new();
    let out = env
        .shux()
        .env("SHELL", "")
        .args(["session", "create", "blankshell", "-d"])
        .output()
        .expect("spawn shux");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    env.sessions.push("blankshell".to_string());

    // The pane is genuinely alive: it answers a write.
    let pane = env.first_pane("blankshell");
    let out = env.run(&[
        "pane",
        "send-keys",
        "-s",
        "blankshell",
        "-p",
        &pane,
        "-t",
        "",
    ]);
    assert!(
        out.status.success(),
        "default pane was dead under a blank $SHELL: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `--cmd` had no `allow_hyphen_values`, so a command starting with a
/// flag-shaped word was refused outright — and clap's suggestion (`-- -n`)
/// points at the argv form, which is a different execution model, silently.
///
/// The assertion is that the value ARRIVES verbatim; whether that particular
/// script then succeeds is the shell's business, not the flag parser's.
#[test]
fn a_cmd_starting_with_a_hyphen_is_taken_as_the_command_not_a_flag() {
    let mut env = Env::new();
    let script = "-n never runs";
    let out = env.run(&["session", "create", "hyphen", "-d", "--cmd", script]);
    let joined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !joined.contains("unexpected argument"),
        "--cmd rejected a hyphen-leading command:\n{joined}"
    );
    assert!(out.status.success(), "{joined}");
    env.sessions.push("hyphen".to_string());

    let panes = env.panes("hyphen", "0");
    assert_eq!(
        panes[0].1.get(2).map(String::as_str),
        Some(script),
        "{:?}",
        panes[0].1
    );
}

// ── round-two follow-ups ────────────────────────────────────────────────
//
// Found by adversarial agents driving the real binary against the first cut of
// the rollback and validation work above. Each was reproduced before it was
// believed.

/// Creating a window focuses it; destroying one hands focus to the session's
/// FIRST window. So the rollback moved the operator's session somewhere else
/// entirely — every later `-w`-less verb then targeted the wrong window.
#[test]
fn a_failed_window_create_leaves_focus_where_it_was() {
    let mut env = Env::new();
    env.ok(&["session", "create", "focus", "-d"]);
    env.sessions.push("focus".to_string());
    env.ok(&["window", "create", "-s", "focus", "-n", "two"]);
    env.ok(&["window", "create", "-s", "focus", "-n", "three"]);

    fn active(e: &Env) -> String {
        e.json(&["window", "list", "-s", "focus"])
            .as_array()
            .expect("windows")
            .iter()
            .find(|w| w["is_active"] == true)
            .and_then(|w| w["title"].as_str())
            .unwrap_or_default()
            .to_string()
    }
    let before = active(&env);
    assert_eq!(before, "three", "setup: newest window should be active");

    let out = env.run(&[
        "window",
        "create",
        "-s",
        "focus",
        "-n",
        "ghost",
        "--",
        "no-such-binary-xyz",
    ]);
    assert!(!out.status.success(), "spawn failure should be an error");

    assert_eq!(
        active(&env),
        before,
        "a failed window create relocated the session's active window"
    );
}

/// Same shape one level down: splitting focuses the new pane, and destroying a
/// pane hands focus to whatever the layout tree yields first.
#[test]
fn a_failed_split_leaves_focus_where_it_was() {
    let mut env = Env::new();
    env.ok(&["session", "create", "pfocus", "-d"]);
    env.sessions.push("pfocus".to_string());
    let first = env.first_pane("pfocus");
    env.ok(&[
        "pane",
        "split",
        "-s",
        "pfocus",
        "-p",
        &first,
        "--cmd",
        &format!("{}; exec sleep 900", probe()),
    ]);

    fn active(e: &Env) -> String {
        e.json(&["pane", "list", "-s", "pfocus"])
            .as_array()
            .expect("panes")
            .iter()
            .find(|p| p["is_focused"] == true)
            .and_then(|p| p["id"].as_str())
            .unwrap_or_default()
            .to_string()
    }
    let before = active(&env);
    assert!(!before.is_empty(), "setup: some pane must be active");

    let out = env.run(&[
        "pane",
        "split",
        "-s",
        "pfocus",
        "-p",
        &before,
        "--",
        "no-such-binary-xyz",
    ]);
    assert!(!out.status.success());

    assert_eq!(active(&env), before, "a failed split moved the active pane");
}

/// `MAX_ARG_STRLEN` counts the terminating NUL, so 131071 is the longest
/// argument that can be exec'd and 131072 is not. The first cut capped at
/// 131072 — and its unit test asserted, greenly, that the length which fails at
/// `execve` was acceptable. This pins the boundary against a real spawn.
#[test]
fn the_argument_length_limit_matches_what_execve_accepts() {
    let mut env = Env::new();
    fn write(work: &std::path::Path, name: &str, len: usize) -> String {
        let path = work.join(name);
        std::fs::write(
            &path,
            serde_json::json!({"name": name, "command": ["/bin/echo", "x".repeat(len)]})
                .to_string(),
        )
        .unwrap();
        format!("@{}", path.display())
    }
    let work = env.work();
    // One under the cap: accepted AND actually spawns.
    let out = env.run(&[
        "rpc",
        "call",
        "session.create",
        "--params",
        &write(&work, "under", 131_071),
        "--format",
        "json",
    ]);
    let joined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "131071 bytes must be accepted and spawnable:\n{joined}"
    );
    env.sessions.push("under".to_string());

    // One over: refused by the parser, never reaching execve.
    let out = env.run(&[
        "rpc",
        "call",
        "session.create",
        "--params",
        &write(&work, "over", 131_072),
        "--format",
        "json",
    ]);
    let joined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "131072 bytes must be refused:\n{joined}"
    );
    assert!(
        joined.contains("cannot exceed"),
        "refusal must name the limit, not fail at execve:\n{joined}"
    );
    assert!(
        !joined.contains("Argument list too long"),
        "the parser let it through to execve:\n{joined}"
    );
}

/// `--dry-run` exists to answer "will this apply succeed?". The argv rule lived
/// only in the daemon, so it answered yes to templates the real run rejects.
#[test]
fn state_apply_dry_run_rejects_what_the_real_run_rejects() {
    let env = Env::new();
    let path = env.work().join("bad.toml");
    std::fs::write(
        &path,
        "[session]\nname = \"dry\"\n\n[[windows]]\ntitle = \"w\"\n\n\
         [[windows.panes]]\ncommand = [\"\"]\n",
    )
    .unwrap();
    let p = path.to_str().unwrap();

    for args in [
        &["state", "apply", p, "--dry-run"][..],
        &["state", "apply", p][..],
    ] {
        let out = env.run(args);
        let joined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !out.status.success(),
            "{args:?} accepted a blank argv:\n{joined}"
        );
        assert!(joined.contains("initial_command"), "{args:?}: {joined}");
    }
}

/// A batch whose panes never started is not a success. `state.apply`
/// deliberately does not roll back, but reporting a green tick and exit 0 let
/// `shux state apply t.toml && shux attach` walk into a session of dead panes.
#[test]
fn state_apply_reports_failure_when_a_pane_does_not_spawn() {
    let mut env = Env::new();
    let path = env.work().join("ghost.toml");
    std::fs::write(
        &path,
        "[session]\nname = \"ghost\"\n\n[[windows]]\ntitle = \"w\"\n\n\
         [[windows.panes]]\ncommand = [\"no-such-binary-xyz\"]\n",
    )
    .unwrap();
    let out = env.run(&["state", "apply", path.to_str().unwrap()]);
    env.sessions.push("ghost".to_string());
    let joined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "apply reported success for a batch whose only pane never spawned:\n{joined}"
    );
    assert!(
        !joined.contains("✓ Applied"),
        "a green tick over a dead pane:\n{joined}"
    );
}

/// A flag is never the program. `--cmd "-n is a valid sed script"` is the
/// example printed in the flag's own help, and it used to title the pane `-n`.
#[test]
fn a_flag_shaped_command_does_not_become_the_pane_title() {
    let mut env = Env::new();
    for (name, script) in [
        ("flaga", "-n is a valid sed script"),
        ("flagb", "--format json"),
        ("flagc", "A=1 -d 10"),
    ] {
        env.ok(&["session", "create", name, "-d", "--cmd", script]);
        env.sessions.push(name.to_string());
        let title = env.json(&["pane", "list", "-s", name])[0]["title"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !title.starts_with('-'),
            "{script:?} produced the flag {title:?} as a pane title"
        );
    }
}

// ── round-three follow-ups ──────────────────────────────────────────────

/// Round two captured focus before the create and wrote it back after — a lost
/// update. A `window focus` issued while the PTY was starting returned success
/// and was then silently reverted. Focus is now only put back if it is still on
/// the entity being undone, so a choice made meanwhile wins.
///
/// Driven as a real race: the failing create runs concurrently with the focus.
/// The margin is wide by construction — the reverting build loses focus on
/// roughly half of these — so the threshold is not measuring jitter.
#[test]
fn a_concurrent_focus_survives_a_rollback() {
    const TRIALS: usize = 16;
    let mut env = Env::new();
    env.ok(&["session", "create", "race", "-d"]);
    env.sessions.push("race".to_string());
    env.ok(&["window", "create", "-s", "race", "-n", "wa"]);
    env.ok(&["window", "create", "-s", "race", "-n", "wb"]);

    fn active(e: &Env) -> String {
        e.json(&["window", "list", "-s", "race"])
            .as_array()
            .expect("windows")
            .iter()
            .find(|w| w["is_active"] == true)
            .and_then(|w| w["title"].as_str())
            .unwrap_or_default()
            .to_string()
    }

    let mut kept = 0;
    for i in 0..TRIALS {
        env.ok(&["window", "focus", "-s", "race", "-w", "wa"]);
        let ghost = format!("ghost{i}");
        // Start the doomed create, then move focus while its PTY is starting.
        let mut child = env
            .shux()
            .args([
                "window",
                "create",
                "-s",
                "race",
                "-n",
                &ghost,
                "--",
                "no-such-binary-xyz",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn create");
        // No delay: both clients must be in flight at once. A sleep here lets
        // the create finish first, and the test then discriminates nothing —
        // the reverting build passes it.
        let focused = env
            .run(&["window", "focus", "-s", "race", "-w", "wb"])
            .status
            .success();
        let _ = child.wait();
        if focused && active(&env) == "wb" {
            kept += 1;
        }
    }
    assert!(
        kept >= TRIALS - 3,
        "a rollback reverted a concurrent focus in {} of {TRIALS} trials",
        TRIALS - kept
    );
}

/// The settled form of the same property, deterministic: a focus that has
/// already landed before the create is never dragged back.
#[test]
fn a_rollback_does_not_clobber_a_focus_change_made_meanwhile() {
    let mut env = Env::new();
    env.ok(&["session", "create", "settled", "-d"]);
    env.sessions.push("settled".to_string());
    env.ok(&["window", "create", "-s", "settled", "-n", "wa"]);
    env.ok(&["window", "create", "-s", "settled", "-n", "wb"]);

    fn active(e: &Env) -> String {
        e.json(&["window", "list", "-s", "settled"])
            .as_array()
            .expect("windows")
            .iter()
            .find(|w| w["is_active"] == true)
            .and_then(|w| w["title"].as_str())
            .unwrap_or_default()
            .to_string()
    }
    env.ok(&["window", "focus", "-s", "settled", "-w", "wb"]);
    let out = env.run(&[
        "window",
        "create",
        "-s",
        "settled",
        "-n",
        "ghost",
        "--",
        "no-such-binary-xyz",
    ]);
    assert!(!out.status.success());
    assert_eq!(active(&env), "wb");
}

/// A successful split legitimately clears zoom. An undone one must not.
#[test]
fn a_failed_split_leaves_the_window_zoomed() {
    let mut env = Env::new();
    env.ok(&["session", "create", "zoomed", "-d"]);
    env.sessions.push("zoomed".to_string());
    let first = env.first_pane("zoomed");
    env.ok(&[
        "pane",
        "split",
        "-s",
        "zoomed",
        "-p",
        &first,
        "--cmd",
        &format!("{}; exec sleep 900", probe()),
    ]);
    env.ok(&["pane", "zoom", "-s", "zoomed", "-p", &first]);

    let zoomed = |e: &Env| -> bool {
        e.json(&["pane", "list", "-s", "zoomed"])
            .as_array()
            .expect("panes")
            .iter()
            .any(|p| p["is_zoomed"] == true)
    };
    assert!(zoomed(&env), "setup: the window should be zoomed");

    let out = env.run(&[
        "pane",
        "split",
        "-s",
        "zoomed",
        "-p",
        &first,
        "--",
        "no-such-binary-xyz",
    ]);
    assert!(!out.status.success());
    assert!(zoomed(&env), "a failed split un-zoomed the window");
}

/// `state.apply` deliberately keeps a pane whose PTY never started — and it
/// focused it, so every `-p`-less verb in that window answered "pane VT not
/// found" against a corpse.
#[test]
fn state_apply_does_not_leave_focus_on_a_pane_that_never_started() {
    let mut env = Env::new();
    // The colour probe lives in a file: TOML basic strings have no `\0`
    // escape, so a `printf '\033[...'` written inline is a parse error.
    let colour = env.work().join("colour.txt");
    std::fs::write(
        &colour,
        "\u{1b}[38;2;120;220;180mTRUECOLOR\u{1b}[0m \u{1b}[38;5;208mINDEXED\u{1b}[0m \u{1b}[34mBASIC\u{1b}[0m\n",
    )
    .unwrap();
    let path = env.work().join("mixed.toml");
    std::fs::write(
        &path,
        format!(
            "[session]\nname = \"mixed\"\n\n[[windows]]\ntitle = \"w\"\n\n\
             [[windows.panes]]\ncommand = [\"/bin/sh\", \"-c\", \"cat {}; exec sleep 900\"]\n\n\
             [[windows.panes]]\ndirection = \"vertical\"\ncommand = [\"no-such-binary-xyz\"]\n",
            colour.display()
        ),
    )
    .unwrap();
    // The apply reports failure (one pane did not spawn) but still commits.
    let _ = env.run(&["state", "apply", path.to_str().unwrap()]);
    env.sessions.push("mixed".to_string());

    let panes = env.json(&["pane", "list", "-s", "mixed"]);
    let focused = panes
        .as_array()
        .expect("panes")
        .iter()
        .find(|p| p["is_focused"] == true)
        .expect("some pane must be focused");
    let cmd = focused["command"]
        .as_array()
        .map(|a| a[0].as_str().unwrap_or_default().to_string())
        .unwrap_or_default();
    assert_ne!(
        cmd, "no-such-binary-xyz",
        "focus was left on the pane that never started"
    );

    // …and the `-p`-less verbs work, which is the point.
    let out = env.run(&["pane", "capture", "-s", "mixed"]);
    assert!(
        out.status.success(),
        "capture on the focused pane failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The human path refused to call a batch of dead panes a success; the JSON
/// path — the one scripts and agents use — still exited 0 over the same batch.
#[test]
fn state_apply_reports_failure_in_json_format_too() {
    let mut env = Env::new();
    let path = env.work().join("json.toml");
    std::fs::write(
        &path,
        "[session]\nname = \"jsonfail\"\n\n[[windows]]\ntitle = \"w\"\n\n\
         [[windows.panes]]\ncommand = [\"no-such-binary-xyz\"]\n",
    )
    .unwrap();
    let out = env.run(&["--format", "json", "state", "apply", path.to_str().unwrap()]);
    env.sessions.push("jsonfail".to_string());
    assert!(
        !out.status.success(),
        "--format json exited 0 over a batch whose only pane never spawned:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// `--dry-run` must reject what the apply rejects for reasons it can decide
/// without the daemon — a window title is one of them.
#[test]
fn state_apply_dry_run_rejects_an_impossible_window_title() {
    let env = Env::new();
    let path = env.work().join("title.toml");
    std::fs::write(
        &path,
        format!(
            "[session]\nname = \"longtitle\"\n\n[[windows]]\ntitle = \"{}\"\n\n\
             [[windows.panes]]\ncommand = [\"/bin/sh\"]\n",
            "t".repeat(300)
        ),
    )
    .unwrap();
    for args in [
        &["state", "apply", path.to_str().unwrap(), "--dry-run"][..],
        &["state", "apply", path.to_str().unwrap()][..],
    ] {
        let out = env.run(args);
        assert!(!out.status.success(), "{args:?} accepted a 300-char title");
    }
}

/// The mirror image, and a regression this task introduced: `title` is a
/// required TOML field, so a template with nothing to say for its first window
/// writes `""`. The apply has always read that as "unspecified" and named the
/// window `1`. Pre-flight validation must not be stricter than the apply it is
/// predicting, or every such template stops working.
#[test]
fn a_template_may_leave_its_first_window_title_blank() {
    let mut env = Env::new();
    let path = env.work().join("blank-title.toml");
    std::fs::write(
        &path,
        "[session]\nname = \"blanktitle\"\n\n[[windows]]\ntitle = \"\"\n\n\
         [[windows.panes]]\ncommand = [\"/bin/sh\", \"-c\", \"exec sleep 900\"]\n",
    )
    .unwrap();

    let dry = env.run(&["state", "apply", path.to_str().unwrap(), "--dry-run"]);
    assert!(
        dry.status.success(),
        "--dry-run rejected a blank first-window title:\n{}{}",
        String::from_utf8_lossy(&dry.stdout),
        String::from_utf8_lossy(&dry.stderr)
    );

    let out = env.run(&["state", "apply", path.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "apply rejected a blank first-window title:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    env.sessions.push("blanktitle".to_string());

    let windows = env.json(&["window", "list", "-s", "blanktitle"]);
    assert_eq!(
        windows[0]["title"].as_str(),
        Some("1"),
        "a blank title should fall back to the default `1`: {windows}"
    );
}

/// A program with an unusual but real name keeps it. Round two's
/// "no alphanumeric character" rule threw `/usr/local/bin/+++` away.
#[test]
fn a_real_program_with_an_unusual_name_keeps_its_title() {
    let mut env = Env::new();
    let bin = env.work().join("+++");
    std::fs::write(&bin, "#!/bin/sh\nexec sleep 900\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    env.ok(&[
        "session",
        "create",
        "plus",
        "-d",
        "--cmd",
        bin.to_str().unwrap(),
    ]);
    env.sessions.push("plus".to_string());
    let title = env.json(&["pane", "list", "-s", "plus"])[0]["title"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(title, "+++", "a real program lost its name");
}

// ── round-four follow-ups ───────────────────────────────────────────────

/// Removing the aggregate cap in round three left `state.apply` — the one
/// spawn path that KEEPS a failed pane — storing a multi-megabyte argv in the
/// graph and echoing it in full from `pane list`. A few of those pushed the
/// response past the frame limit and every read of that session died with
/// "early eof", recoverable only by killing it.
#[test]
fn an_argv_larger_than_shux_will_carry_never_reaches_the_graph() {
    let mut env = Env::new();
    let arg = "x".repeat(100_000);
    let args: Vec<String> = std::iter::repeat_n(arg, 30).collect();
    let quoted: Vec<String> = args.iter().map(|a| format!("\"{a}\"")).collect();
    let path = env.work().join("big.toml");
    std::fs::write(
        &path,
        format!(
            "[session]\nname = \"bigargv\"\n\n[[windows]]\ntitle = \"w\"\n\n\
             [[windows.panes]]\ncommand = [\"/bin/sh\", \"-c\", \"exec sleep 900\"]\n\n\
             [[windows.panes]]\ndirection = \"vertical\"\ncommand = [\"/bin/echo\", {}]\n",
            quoted.join(", ")
        ),
    )
    .unwrap();

    // Rejected before anything commits — and by `--dry-run` identically.
    for args in [
        &["state", "apply", path.to_str().unwrap(), "--dry-run"][..],
        &["state", "apply", path.to_str().unwrap()][..],
    ] {
        let out = env.run(args);
        let joined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!out.status.success(), "{args:?} accepted it:\n{joined}");
        assert!(joined.contains("stores and reports"), "{args:?}: {joined}");
    }
    assert!(
        env.json(&["session", "list"])["sessions"]
            .as_array()
            .map(Vec::is_empty)
            .unwrap_or(true),
        "a rejected apply still committed the session"
    );
    env.sessions.push("bigargv".to_string());
}

/// The rescue asked "is this pane absent from THIS batch's failures?", so a
/// corpse left by an earlier apply counted as a healthy sibling and focus
/// landed on it — reproducing exactly what the rescue was written to prevent.
///
/// `state.apply` is the only way to accumulate corpses: `pane.split` the RPC
/// rolls one back, but a `split_pane` OP does not. The window is stacked with
/// several on purpose — the old code chose by `HashMap` iteration order, so one
/// corpse among two panes was a coin flip.
#[test]
fn the_focus_rescue_skips_a_corpse_left_by_an_earlier_apply() {
    let mut env = Env::new();
    let colour = env.work().join("colour.txt");
    std::fs::write(
        &colour,
        "\u{1b}[38;2;120;220;180mTRUECOLOR\u{1b}[0m \u{1b}[38;5;208mINDEXED\u{1b}[0m \u{1b}[34mBASIC\u{1b}[0m\n",
    )
    .unwrap();
    env.ok(&[
        "session",
        "create",
        "corpse",
        "-d",
        "--cmd",
        &format!("cat {}; exec sleep 900", colour.display()),
    ]);
    env.sessions.push("corpse".to_string());
    let live = env.first_pane("corpse");

    // Six failing `split_pane` ops, each leaving a corpse beside the live pane.
    for i in 0..6 {
        let ops = serde_json::json!({"ops": [{
            "op": "split_pane",
            "target": live,
            "direction": "vertical",
            "ratio": 0.5,
            "command": [format!("no-such-binary-{i}")],
        }]})
        .to_string();
        let _ = env.rpc("state.apply", &ops);
    }

    let panes = env.json(&["pane", "list", "-s", "corpse"]);
    assert!(
        panes.as_array().map(Vec::len).unwrap_or(0) >= 6,
        "setup: the corpses should have accumulated"
    );
    let focused = panes
        .as_array()
        .expect("panes")
        .iter()
        .find(|p| p["is_focused"] == true)
        .expect("some pane focused");
    assert_eq!(
        focused["id"].as_str().unwrap_or_default(),
        live,
        "focus is not on the one pane with a live PTY"
    );
    // …which is the point: the `-p`-less verbs have to work.
    let out = env.run(&["pane", "capture", "-s", "corpse"]);
    assert!(
        out.status.success(),
        "the focused pane does not answer:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The other half of "alive", and the half the test above cannot see because
/// its live pane runs `exec sleep 900` forever.
///
/// A pane that has **exited** is still perfectly usable: shux keeps its grid
/// and scrollback on purpose, so `pane capture` and `pane snapshot` answer for
/// a short-lived command long after it finished. Only the PTY write side is
/// torn down. Keying the rescue on the write side therefore skipped exactly
/// the sibling a build template leaves behind — `["/bin/echo", "…"]` beside a
/// typo'd editor — and focus stayed on the corpse.
#[test]
fn the_focus_rescue_accepts_a_sibling_that_has_already_finished() {
    let mut env = Env::new();
    let colour = env.work().join("finished-colour.txt");
    std::fs::write(
        &colour,
        "\u{1b}[38;2;120;220;180mTRUECOLOR\u{1b}[0m \u{1b}[38;5;208mINDEXED\u{1b}[0m \u{1b}[34mBASIC\u{1b}[0m\n",
    )
    .unwrap();
    let path = env.work().join("finished.toml");
    std::fs::write(
        &path,
        format!(
            "[session]\nname = \"finished\"\n\n[[windows]]\ntitle = \"build\"\n\n\
             [[windows.panes]]\ncommand = [\"/bin/cat\", \"{}\"]\n\n\
             [[windows.panes]]\ndirection = \"vertical\"\n\
             command = [\"no-such-editor-zz\", \"src/main.rs\"]\n",
            colour.display()
        ),
    )
    .unwrap();
    // One pane finishes, one never starts. The apply reports the failure and
    // still commits — that part is deliberate.
    let _ = env.run(&["state", "apply", path.to_str().unwrap()]);
    env.sessions.push("finished".to_string());

    let panes = env.wait_for_exited_pane("finished", "/bin/cat");
    let finished = panes
        .as_array()
        .expect("panes")
        .iter()
        .find(|p| p["exit_status"].is_number() && p["command"].to_string().contains("/bin/cat"))
        .expect("the `cat` pane should have exited")["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let focused = panes
        .as_array()
        .expect("panes")
        .iter()
        .find(|p| p["is_focused"] == true)
        .expect("some pane focused")["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        focused, finished,
        "focus stayed on the pane that never started instead of the finished sibling: {panes}"
    );

    // The whole point of the rescue: a `-p`-less verb has to answer, and it has
    // to answer with the finished command's output — colour included.
    let out = env.run(&["pane", "capture", "-s", "finished"]);
    assert!(
        out.status.success(),
        "the focused pane does not answer:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    for needle in ["TRUECOLOR", "INDEXED", "BASIC"] {
        assert!(
            text.contains(needle),
            "the finished pane's retained output is missing {needle}:\n{text}"
        );
    }
}

/// Round three's ignorable-codepoint rule stripped the variation selectors,
/// which are mandatory in the very emoji sequences the ZWJ exception exists to
/// protect: `❤️‍🔥` came back as `❤` and `1️⃣` as a bare `1`.
#[test]
fn emoji_sequences_survive_the_title_sanitizer() {
    let mut env = Env::new();
    env.ok(&["session", "create", "emoji", "-d"]);
    env.sessions.push("emoji".to_string());
    for (title, why) in [
        ("\u{2764}\u{fe0f}\u{200d}\u{1f525} hot", "heart on fire"),
        ("\u{1f3f3}\u{fe0f}\u{200d}\u{1f308} pride", "rainbow flag"),
        ("1\u{fe0f}\u{20e3} first", "keycap one"),
        ("\u{26a0}\u{fe0f} prod", "warning sign"),
    ] {
        let out = env.json(&["window", "create", "-s", "emoji", "-n", title]);
        assert_eq!(
            out["title"].as_str().unwrap_or_default(),
            title,
            "{why} was altered by the sanitizer"
        );
    }
}

/// A command whose name sanitizes to nothing produced a pane with a blank
/// border and a blank status bar — round three widened both what counts as a
/// program name and what the sanitizer removes.
#[test]
fn a_command_that_sanitizes_to_nothing_still_gets_a_title() {
    let mut env = Env::new();
    env.ok(&["session", "create", "blankname", "-d", "--cmd", "\u{00ad}"]);
    env.sessions.push("blankname".to_string());
    let title = env.json(&["pane", "list", "-s", "blankname"])[0]["title"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(!title.is_empty(), "pane was left with no title at all");

    // The cwd is the fallback, so it has to hold up too. `--cwd /` is an
    // ordinary invocation and `Path::file_name` is `None` for it, which left
    // the border and the status bar blank in exactly the same way.
    env.ok(&["session", "create", "rootcwd", "-d", "--cwd", "/"]);
    env.sessions.push("rootcwd".to_string());
    let title = env.json(&["pane", "list", "-s", "rootcwd"])[0]["title"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !title.is_empty(),
        "a pane opened at the filesystem root was left with no title"
    );
}

/// `state.apply` is the one path where an oversized argv can land, and it was
/// the one path whose failure said nothing useful.
#[test]
fn state_apply_spawn_failures_carry_the_same_diagnosis_as_the_rpcs() {
    let mut env = Env::new();
    let path = env.work().join("dir.toml");
    std::fs::write(
        &path,
        format!(
            "[session]\nname = \"diag\"\n\n[[windows]]\ntitle = \"w\"\n\n\
             [[windows.panes]]\ncommand = [\"{}\"]\n",
            env.work().display()
        ),
    )
    .unwrap();
    let out = env.run(&["state", "apply", path.to_str().unwrap()]);
    env.sessions.push("diag".to_string());
    let joined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        joined.contains("not an executable file"),
        "a directory as argv[0] got no useful diagnosis:\n{joined}"
    );
}
