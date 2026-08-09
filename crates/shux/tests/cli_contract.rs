//! End-to-end CLI contract tests.
//!
//! These are defects in what the *shipped binary* prints or accepts, so every
//! case here drives `CARGO_BIN_EXE_shux` against a real daemon in an isolated
//! `XDG_RUNTIME_DIR`. A unit test on the formatting helpers would have passed
//! throughout: #133 lives in the gap between `style::print_error` and `main`'s
//! `Termination` impl.

use std::path::PathBuf;
use std::process::{Command, Output};

/// Keeps a pane alive past the assertion.
const PARK: &str = "exec sleep 900";

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
            .env_remove("RUST_BACKTRACE")
            .env("NO_COLOR", "1")
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
            "{args:?} unexpectedly failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// Run expecting a non-zero exit; returns stderr.
    #[track_caller]
    fn fails(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            !out.status.success(),
            "{args:?} unexpectedly SUCCEEDED:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
        String::from_utf8_lossy(&out.stderr).to_string()
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let mut full = vec!["--format", "json"];
        full.extend_from_slice(args);
        let out = self.ok(&full);
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("not json ({e}): {out}"))
    }

    #[track_caller]
    fn session(&mut self, name: &str) -> String {
        self.ok(&["session", "create", name, "-d", "--cmd", PARK]);
        self.sessions.push(name.to_string());
        name.to_string()
    }

    fn pane_count(&self, session: &str) -> usize {
        self.json(&["pane", "list", "-s", session])
            .as_array()
            .map(|a| a.len())
            .unwrap_or(0)
    }

    fn write_template(&self, name: &str, body: &str) -> PathBuf {
        let p = self.work().join(name);
        std::fs::write(&p, body).expect("write template");
        p
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

// ── #133: every CLI error prints twice, the second time with a backtrace ────

#[test]
fn a_failing_command_prints_its_error_exactly_once() {
    let env = Env::new();
    let stderr = env.fails(&["session", "kill", "no-such-session"]);

    let hits = stderr.matches("not found").count();
    assert_eq!(
        hits, 1,
        "expected the error once, saw it {hits}x:\n---\n{stderr}\n---"
    );
    assert!(
        stderr.contains("no-such-session"),
        "the one line must still name the session:\n{stderr}"
    );
}

#[test]
fn a_failing_command_prints_no_backtrace_by_default() {
    let env = Env::new();
    let stderr = env.fails(&["session", "kill", "no-such-session"]);

    assert!(
        !stderr.contains("Stack backtrace"),
        "a user error must not carry a backtrace:\n---\n{stderr}\n---"
    );
    assert!(
        !stderr.contains("anyhow"),
        "dependency paths must not leak into a user error:\n---\n{stderr}\n---"
    );
    // anyhow's `Termination` impl is what adds the second copy; its prefix is
    // the cheapest proof that path is no longer reached.
    assert!(
        !stderr.contains("Error:"),
        "the raw `Error:` re-print must be gone:\n---\n{stderr}\n---"
    );
}

#[test]
fn a_backtrace_is_available_behind_rust_backtrace() {
    let env = Env::new();
    let out = env
        .shux()
        .args(["session", "kill", "no-such-session"])
        .env("RUST_BACKTRACE", "1")
        .output()
        .expect("spawn shux");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    assert!(!out.status.success(), "should still fail");
    assert!(
        stderr.contains("Stack backtrace") || stderr.contains("stack backtrace"),
        "RUST_BACKTRACE=1 is the documented opt-in; it must still produce one:\n---\n{stderr}\n---"
    );
    // Even opted in, the human line is printed once.
    assert_eq!(
        stderr.matches("✗").count(),
        1,
        "the human line stays single even with the opt-in:\n---\n{stderr}\n---"
    );
}

#[test]
fn several_distinct_error_paths_all_print_once() {
    let mut env = Env::new();
    let s = env.session("errpaths");

    for args in [
        vec!["session", "kill", "no-such-session"],
        vec!["pane", "list", "-s", "no-such-session"],
        vec!["pane", "capture", "-s", &s, "-p", "ffffffff"],
        vec!["window", "kill", "-s", &s, "-w", "no-such-window"],
    ] {
        let stderr = env.fails(&args);
        assert!(
            !stderr.contains("Stack backtrace") && !stderr.contains("Error:"),
            "{args:?} still double-prints:\n---\n{stderr}\n---"
        );
        assert_eq!(
            stderr.matches("✗").count(),
            1,
            "{args:?} printed the marker more than once:\n---\n{stderr}\n---"
        );
    }
}
