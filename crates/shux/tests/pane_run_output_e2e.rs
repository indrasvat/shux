//! `pane run` returns exactly what the command wrote: not the prompt, not the
//! echoed line or its markers, not an earlier command's output (issue #259).
//! Driven through the real daemon, in an interactive bash with a two-line
//! coloured prompt.

use std::path::PathBuf;
use std::process::Command;

struct Env {
    bin: PathBuf,
    root: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("temp root");
        std::fs::create_dir_all(root.path().join("runtime")).expect("mkdir");
        // A two-line prompt in colour, the shape that used to leak into stdout.
        std::fs::write(
            root.path().join("rc"),
            "PS1=$'\\e[35mpane-run-prompt\\e[0m\\n\\e[32m$\\e[0m '\n",
        )
        .expect("rcfile");
        Self {
            bin: PathBuf::from(env!("CARGO_BIN_EXE_shux")),
            root,
        }
    }

    fn shux(&self) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.env("XDG_RUNTIME_DIR", self.root.path().join("runtime"))
            .env_remove("SHUX_SOCKET")
            .env_remove("NO_COLOR")
            .current_dir(self.root.path());
        cmd
    }

    fn run_in_pane(&self, command: &str) -> serde_json::Value {
        let out = self
            .shux()
            .args([
                "--format",
                "json",
                "pane",
                "run",
                "-s",
                "pr",
                "--timeout",
                "30",
                "-c",
                command,
            ])
            .output()
            .expect("spawn shux");
        assert!(
            out.status.success(),
            "pane run failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).expect("json")
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self.shux().args(["daemon", "stop"]).output();
    }
}

#[test]
fn pane_run_returns_only_what_the_command_wrote() {
    let env = Env::new();
    let rc = env.root.path().join("rc");
    let created = env
        .shux()
        .args([
            "session",
            "create",
            "pr",
            "-d",
            "--",
            "bash",
            "--noprofile",
            "--rcfile",
        ])
        .arg(&rc)
        .arg("-i")
        .output()
        .expect("spawn shux");
    assert!(created.status.success(), "session create failed");
    let ready = env
        .shux()
        .args([
            "pane",
            "wait-for",
            "-s",
            "pr",
            "--text",
            "pane-run-prompt",
            "--timeout-ms",
            "30000",
        ])
        .status()
        .expect("wait-for");
    assert!(ready.success(), "the shell never drew its prompt");

    // Long enough to wrap at 80 columns; truecolor, 256-colour and basic colour.
    let pad = "x".repeat(120);
    let first = env.run_in_pane(&format!(
        "PAD={pad}; printf '\\033[38;2;10;200;30mrow 1\\033[0m\\n\\033[38;5;208mrow 2\\033[0m\\n\\033[31mrow 3\\033[0m\\n'"
    ));
    assert_eq!(first["stdout"], "row 1\nrow 2\nrow 3\n", "{first}");
    assert_eq!(first["exit_code"], 0, "{first}");

    let second = env.run_in_pane("ls /nonexistent-pane-run-dir");
    let stdout = second["stdout"].as_str().unwrap_or_default();
    assert!(
        stdout.starts_with("ls: ") && stdout.contains("nonexistent-pane-run-dir"),
        "the second call's output is not just its own error: {second}"
    );
    assert!(
        !stdout.contains("row 1")
            && !stdout.contains("pane-run-prompt")
            && !stdout.contains("SHUX_"),
        "the second call carried the first call's output, the prompt or a marker: {second}"
    );
    assert_ne!(second["exit_code"], 0, "{second}");
}

/// A command started while another runs in the same pane is refused: its
/// typed line would be echoed into the running command's output.
#[test]
fn a_second_command_waits_its_turn_instead_of_polluting_the_first() {
    let env = Env::new();
    let rc = env.root.path().join("rc");
    assert!(
        env.shux()
            .args([
                "session",
                "create",
                "pr",
                "-d",
                "--",
                "bash",
                "--noprofile",
                "--rcfile"
            ])
            .arg(&rc)
            .arg("-i")
            .status()
            .expect("spawn shux")
            .success()
    );
    assert!(
        env.shux()
            .args([
                "pane",
                "wait-for",
                "-s",
                "pr",
                "--text",
                "pane-run-prompt",
                "--timeout-ms",
                "30000"
            ])
            .status()
            .expect("wait-for")
            .success()
    );

    let started = env
        .shux()
        .args([
            "--format",
            "json",
            "pane",
            "run",
            "-s",
            "pr",
            "--async",
            "-c",
            "sleep 1; echo first",
        ])
        .output()
        .expect("spawn shux");
    let started: serde_json::Value = serde_json::from_slice(&started.stdout).expect("json");
    let first_id = started["command_id"]
        .as_str()
        .expect("command id")
        .to_string();

    let second = env
        .shux()
        .args([
            "--format",
            "json",
            "pane",
            "run",
            "-s",
            "pr",
            "--timeout",
            "10",
            "-c",
            "echo second",
        ])
        .output()
        .expect("spawn shux");

    // The first command's own result, once it has finished.
    let params = serde_json::json!({ "command_id": first_id }).to_string();
    let deadline = std::time::Instant::now() + shux_testkit::BUDGET;
    let first = loop {
        let out = env
            .shux()
            .args([
                "--format",
                "json",
                "rpc",
                "call",
                "pane.command_status",
                "--params",
                &params,
            ])
            .output()
            .expect("spawn shux");
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
        let r = v.get("result").cloned().unwrap_or(v);
        if r["state"] != "running" {
            break r;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the first command never finished"
        );
        std::thread::yield_now();
    };
    assert_eq!(
        first["stdout"], "first\n",
        "the first command's output was polluted: {first}"
    );
    assert!(
        !second.status.success(),
        "a second command was accepted while the first ran: {}",
        String::from_utf8_lossy(&second.stdout)
    );
}
