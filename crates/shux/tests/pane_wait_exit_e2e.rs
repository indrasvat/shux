//! End to end: `shux pane wait-exit` and `--wait` on the verbs that spawn a
//! program (issue #250), against the shipped binary — a real daemon in an
//! isolated `XDG_RUNTIME_DIR`, real PTYs, real shells.
//!
//! Each program prints a colour probe (truecolor + 256-indexed + basic ANSI)
//! before it ends, and the pane's frozen screen is checked for it after the
//! wait returns: the exit is reported with the output kept, not instead of it.

use std::path::PathBuf;
use std::process::{Command, Output};

const PROBE: &str = r"printf '\033[38;2;120;220;180mTRUECOLOR\033[0m \033[38;5;208mINDEXED\033[0m \033[34mBASIC\033[0m\n'";

struct Env {
    bin: PathBuf,
    root: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("temp root");
        for sub in ["runtime", "config/shux", "work"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
        }
        let env = Self {
            bin: PathBuf::from(env!("CARGO_BIN_EXE_shux")),
            root,
        };
        env.ok(&["session", "create", "w", "-d", "--", "sleep", "900"]);
        env
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
        assert!(out.status.success(), "{args:?} failed: {}", show(&out));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// `--format json` `args`: the exit code and the parsed stdout.
    #[track_caller]
    fn json(&self, args: &[&str]) -> (i32, serde_json::Value) {
        let mut full = vec!["--format", "json"];
        full.extend_from_slice(args);
        let out = self.run(&full);
        let v = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{args:?}: not json ({e}): {}", show(&out)));
        (out.status.code().expect("exited, not signalled"), v)
    }

    /// A new pane in session `w` running `script` under `sh -c`; its id.
    #[track_caller]
    fn split(&self, script: &str) -> String {
        let (code, v) = self.json(&["pane", "split", "-s", "w", "--", "sh", "-c", script]);
        assert_eq!(code, 0, "{v}");
        v["pane"]["id"].as_str().expect("pane id").to_string()
    }

    #[track_caller]
    fn wait_exit(&self, pane: &str, extra: &[&str]) -> (i32, serde_json::Value) {
        let mut args = vec!["pane", "wait-exit", pane];
        args.extend_from_slice(extra);
        self.json(&args)
    }

    #[track_caller]
    fn assert_probe(&self, pane: &str) {
        let screen = self.ok(&["pane", "capture", "-s", "w", "-p", pane]);
        for word in ["TRUECOLOR", "INDEXED", "BASIC"] {
            assert!(screen.contains(word), "{word} missing from:\n{screen}");
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self.run(&["session", "kill", "w"]);
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

fn show(out: &Output) -> String {
    format!(
        "exit {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn wait_exit_reports_the_programs_own_code() {
    let env = Env::new();
    for code in [0, 7, 143] {
        let pane = env.split(&format!("{PROBE}; sleep 0.3; exit {code}"));
        let (exit, v) = env.wait_exit(&pane, &["--timeout", "30s"]);
        assert_eq!(exit, 0, "the wait itself succeeded: {v}");
        let r = &v["result"];
        assert_eq!(r["outcome"], "exited", "{v}");
        assert_eq!(r["exit_code"], code, "{v}");
        assert!(r.get("signal").is_none(), "a code is not a signal: {v}");
        env.assert_probe(&pane);
    }
}

#[test]
fn wait_exit_reports_a_signal_death_as_the_signal() {
    let env = Env::new();
    let pane = env.split(&format!("{PROBE}; kill -TERM $$"));
    let (exit, v) = env.wait_exit(&pane, &["--timeout", "30s"]);
    assert_eq!(exit, 0, "{v}");
    let r = &v["result"];
    assert_eq!(
        (&r["signal"], &r["signal_name"]),
        (&15.into(), &"SIGTERM".into()),
        "{v}"
    );
    assert!(r.get("exit_code").is_none(), "-1 is not an exit code: {v}");
    env.assert_probe(&pane);
    let (_, list) = env.json(&["pane", "list", "-s", "w"]);
    let row = list
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == pane.as_str())
        .unwrap();
    assert_eq!(
        row["exit_signal"], 15,
        "pane list carries the signal: {row}"
    );
}

/// Revokes its terminal and keeps running, so shux sees the PTY close under
/// a live program and has to end it. Only `revoke(2)` can do that: shux holds
/// a slave fd while the child lives, so closing stdio is not enough.
#[cfg(target_os = "macos")]
const OUTLIVES_ITS_TERMINAL: &str = r#"
import ctypes, os, signal, sys, time
signal.signal(signal.SIGHUP, signal.SIG_IGN)
ctypes.CDLL(None).revoke(os.ttyname(0).encode())
signal.signal(signal.SIGHUP, signal.SIG_DFL)
null = os.open("/dev/null", os.O_RDWR)
for fd in (0, 1, 2):
    os.dup2(null, fd)
if sys.argv[1:] == ["term-on-hup"]:
    signal.signal(signal.SIGHUP, lambda *_: os.kill(os.getpid(), signal.SIGTERM))
time.sleep(30)
"#;

#[cfg(target_os = "macos")]
#[test]
fn a_program_shux_had_to_end_is_marked_as_ended_by_shux() {
    let env = Env::new();
    let (exit, v) = env.json(&[
        "pane",
        "split",
        "-s",
        "w",
        "--wait",
        "--wait-timeout",
        "20s",
        "--",
        "python3",
        "-I",
        "-c",
        OUTLIVES_ITS_TERMINAL,
    ]);
    assert_eq!(exit, 0, "{v}");
    let r = &v["result"];
    assert!(r["signal"].is_number(), "{v}");
    assert_eq!(
        r["by_shux"], true,
        "shux's own signal read as the program's: {v}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_signal_the_program_raised_itself_is_not_put_on_shux() {
    let env = Env::new();
    let (exit, v) = env.json(&[
        "pane",
        "split",
        "-s",
        "w",
        "--wait",
        "--wait-timeout",
        "20s",
        "--",
        "python3",
        "-I",
        "-c",
        OUTLIVES_ITS_TERMINAL,
        "term-on-hup",
    ]);
    assert_eq!(exit, 0, "{v}");
    let r = &v["result"];
    assert_eq!(r["signal_name"], "SIGTERM", "{v}");
    assert!(
        r.get("by_shux").is_none(),
        "shux sent SIGHUP, not SIGTERM: {v}"
    );
}

#[test]
fn wait_exit_on_an_exited_pane_answers_without_waiting() {
    let env = Env::new();
    let pane = env.split(&format!("{PROBE}; exit 4"));
    assert_eq!(env.wait_exit(&pane, &["--timeout", "30s"]).0, 0);
    // A zero deadline only passes if the exit is already on record.
    let (exit, v) = env.wait_exit(&pane, &["--timeout", "0"]);
    assert_eq!(exit, 0, "{v}");
    assert_eq!(v["result"]["exit_code"], 4, "{v}");
}

#[test]
fn wait_exit_on_an_unknown_pane_exits_2() {
    let env = Env::new();
    for id in ["0000ffff", "00000000-0000-4000-8000-000000000000"] {
        let (exit, v) = env.wait_exit(id, &["--timeout", "0"]);
        assert_eq!(exit, 2, "{id}: {v}");
        assert!(v["error"].is_object(), "{id}: {v}");
    }
}

#[test]
fn a_program_that_reports_done_but_keeps_running_has_not_exited() {
    let env = Env::new();
    let pane = env.split(&format!(
        r"{PROBE}; printf '\033]7501;state=done\033\\'; sleep 900"
    ));
    // The report landed: wait-status sees it.
    let (status, v) = env.json(&[
        "pane",
        "wait-status",
        &pane,
        "--state",
        "done",
        "--timeout",
        "30s",
    ]);
    assert_eq!(status, 0, "{v}");
    let (exit, v) = env.wait_exit(&pane, &["--timeout", "500ms"]);
    assert_eq!(exit, 1, "a running program is a timeout: {v}");
    assert_eq!(v["result"]["outcome"], "timeout", "{v}");
    env.assert_probe(&pane);
}

#[test]
fn wait_on_every_spawning_verb_returns_the_exit_and_what_it_created() {
    let env = Env::new();
    let script = format!("{PROBE}; exit 7");
    let cases: [(&[&str], &str); 3] = [
        (&["pane", "split", "-s", "w", "--wait"], "/created/pane/id"),
        (
            &["window", "create", "-s", "w", "--wait"],
            "/created/pane_id",
        ),
        (
            &["session", "create", "batch", "--wait"],
            "/created/pane_id",
        ),
    ];
    for (verb, pane_at) in cases {
        let mut args = verb.to_vec();
        args.extend_from_slice(&["--", "sh", "-c", &script]);
        let (exit, v) = env.json(&args);
        assert_eq!(exit, 0, "{verb:?}: {v}");
        let r = &v["result"];
        assert_eq!(
            (&r["outcome"], &r["exit_code"]),
            (&"exited".into(), &7.into()),
            "{verb:?}: {v}"
        );
        let pane = r
            .pointer(pane_at)
            .and_then(|p| p.as_str())
            .expect("created pane");
        assert_eq!(
            r["pane_id"], pane,
            "{verb:?}: waited on the pane it made: {v}"
        );
    }
    env.ok(&["session", "kill", "batch"]);

    let (exit, v) = env.json(&[
        "pane",
        "split",
        "-s",
        "w",
        "--wait",
        "--wait-timeout",
        "300ms",
        "--",
        "sleep",
        "900",
    ]);
    assert_eq!(exit, 1, "{v}");
    assert!(
        v["result"]["created"]["pane"]["id"].is_string(),
        "ids survive a timeout: {v}"
    );
}

#[test]
fn wait_exit_text_says_how_it_ended() {
    let env = Env::new();
    let pane = env.split("exit 7");
    let out = env.run(&["pane", "wait-exit", &pane, "--timeout", "30s"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}", show(&out));
    assert!(text.contains("exited 7"), "{text}");
}

#[test]
fn a_wait_whose_create_fails_is_one_json_error_and_never_exit_1() {
    let env = Env::new();
    let cases: [&[&str]; 4] = [
        &["pane", "split", "-s", "nosuch", "--wait", "--", "true"],
        &[
            "pane",
            "split",
            "-s",
            "w",
            "--wait",
            "--",
            "/nonexistent/prog",
        ],
        &["session", "create", "w", "--wait", "--", "true"],
        &["window", "create", "-s", "nosuch", "--wait", "--", "true"],
    ];
    for args in cases {
        let (code, v) = env.json(args);
        assert_eq!(
            code, 2,
            "{args:?}: a refused create is 2, never the timeout's 1: {v}"
        );
        assert!(v["error"].is_object(), "{args:?}: {v}");
    }
    let out = env.run(&[
        "--socket",
        "/nonexistent-dir/x.sock",
        "--format",
        "json",
        "pane",
        "split",
        "-s",
        "w",
        "--wait",
        "--",
        "true",
    ]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "no daemon to reach: {}",
        show(&out)
    );
}

#[test]
fn a_split_runs_where_its_source_pane_is() {
    let env = Env::new();
    let proj = env.root.path().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let proj = proj.canonicalize().unwrap();
    let dir = proj.to_str().unwrap();
    env.ok(&[
        "session", "create", "p", "-d", "--cwd", dir, "--", "sleep", "900",
    ]);
    let check = format!(r#"test "$(pwd -P)" = '{dir}'"#);
    let (exit, v) = env.json(&[
        "pane", "split", "-s", "p", "--wait", "--", "sh", "-c", &check,
    ]);
    assert_eq!(exit, 0, "{v}");
    assert_eq!(v["result"]["exit_code"], 0, "ran outside {dir}: {v}");
    env.ok(&["session", "kill", "p"]);
}

#[test]
fn text_and_plain_wait_show_what_was_created_and_how_it_ended() {
    let env = Env::new();
    let out = env.run(&[
        "window", "create", "-s", "w", "-n", "build", "--wait", "--", "sh", "-c", "exit 7",
    ]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}", show(&out));
    assert!(
        text.contains("build") && text.contains("exited 7"),
        "{text}"
    );

    let out = env.run(&[
        "--format",
        "plain",
        "pane",
        "split",
        "-s",
        "w",
        "--wait",
        "--",
        "sh",
        "-c",
        "kill -TERM $$",
    ]);
    let last = String::from_utf8_lossy(&out.stdout)
        .lines()
        .last()
        .unwrap_or_default()
        .to_string();
    let cols: Vec<&str> = last.split('\t').collect();
    assert_eq!(cols.len(), 4, "{last:?}");
    assert_eq!(
        (cols[0], cols[2], cols[3]),
        ("exited", "", "SIGTERM"),
        "{last:?}"
    );
}
