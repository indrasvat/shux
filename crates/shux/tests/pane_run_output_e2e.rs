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

    /// An interactive bash with the two-line prompt, drawn and ready.
    fn start_shell(&self) {
        let created = self
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
            .arg(self.root.path().join("rc"))
            .arg("-i")
            .output()
            .expect("spawn shux");
        assert!(created.status.success(), "session create failed");
        let ready = self
            .shux()
            .args(["pane", "wait-for", "-s", "pr", "--text", "pane-run-prompt"])
            .args(["--timeout-ms", "30000"])
            .status()
            .expect("wait-for");
        assert!(ready.success(), "the shell never drew its prompt");
    }

    fn run_raw(&self, flags: &[&str], command: &str) -> std::process::Output {
        self.shux()
            .args(["--format", "json", "pane", "run", "-s", "pr"])
            .args(flags)
            .args(["-c", command])
            .output()
            .expect("spawn shux")
    }

    fn run_in_pane(&self, command: &str) -> serde_json::Value {
        let out = self.run_raw(&["--timeout", "30"], command);
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
    env.start_shell();

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

/// A command started while another runs in the same pane is refused, naming
/// the one running. Once the first finishes, the same command is accepted.
#[test]
fn a_second_command_is_refused_while_the_first_runs_then_accepted() {
    let env = Env::new();
    let gate = env.root.path().join("gate");
    assert!(
        Command::new("mkfifo")
            .arg(&gate)
            .status()
            .expect("mkfifo")
            .success()
    );
    env.start_shell();

    // The first command blocks reading the gate until the test opens it.
    let started = env.run_raw(
        &["--async"],
        &format!("read -r _ < '{}'; echo first", gate.display()),
    );
    let started: serde_json::Value = serde_json::from_slice(&started.stdout).expect("json");
    let first_id = started["command_id"]
        .as_str()
        .expect("command id")
        .to_string();

    let refused = env.run_raw(&["--timeout", "30"], "echo second");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        !refused.status.success() && said.contains("still running") && said.contains(&first_id),
        "the second command was not refused as busy with {first_id}: {said}"
    );

    std::fs::write(&gate, "go\n").expect("open the gate");
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

    let accepted = env.run_in_pane("echo second");
    assert_eq!(accepted["stdout"], "second\n", "{accepted}");
}

/// A command that timed out is still reading the pane: the next command is
/// refused rather than typed into it, until the first is cancelled.
#[test]
fn a_timed_out_command_holds_the_pane_until_cancelled() {
    let env = Env::new();
    env.start_shell();

    let timed_out = env.run_raw(&["--timeout", "1"], "read -r x; echo got:$x");
    let timed_out: serde_json::Value = serde_json::from_slice(&timed_out.stdout).expect("json");
    assert_eq!(timed_out["state"], "timed_out", "{timed_out}");
    let first_id = timed_out["command_id"].as_str().expect("command id");

    let refused = env.run_raw(&["--timeout", "30"], "echo second");
    let said = String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success() && said.contains(first_id),
        "a second command was typed into the timed-out one: {}{said}",
        String::from_utf8_lossy(&refused.stdout)
    );
    // Waiting cannot free the pane: the refusal must say so.
    assert!(
        said.contains("timed_out") && said.contains("cancel") && !said.contains("wait for it"),
        "the refusal sends the caller to wait on a command that already timed out: {}{said}",
        String::from_utf8_lossy(&refused.stdout)
    );

    let params = serde_json::json!({ "command_id": first_id }).to_string();
    let cancelled = env
        .shux()
        .args(["--format", "json", "rpc", "call", "pane.command_cancel"])
        .args(["--params", &params])
        .output()
        .expect("spawn shux");
    assert!(
        cancelled.status.success(),
        "{}",
        String::from_utf8_lossy(&cancelled.stderr)
    );
    let cancelled: serde_json::Value = serde_json::from_slice(&cancelled.stdout).expect("json");
    let cancelled = cancelled.get("result").unwrap_or(&cancelled);
    assert_eq!(
        cancelled["state"], "timed_out",
        "cancel and status disagree on the command's state: {cancelled}"
    );
    let accepted = env.run_in_pane("echo second");
    assert_eq!(accepted["stdout"], "second\n", "{accepted}");
}
