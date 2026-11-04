//! `pane wait-status` waits for one named report, not for whatever `done` a
//! previous command left behind (issue #260). Driven through the real daemon,
//! in an interactive bash whose commands report over OSC 7501.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

struct Env {
    bin: PathBuf,
    root: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("temp root");
        std::fs::create_dir_all(root.path().join("runtime")).expect("mkdir");
        std::fs::write(root.path().join("rc"), "PS1='wait-status-prompt $ '\n").expect("rcfile");
        Self {
            bin: PathBuf::from(env!("CARGO_BIN_EXE_shux")),
            root,
        }
    }

    fn shux(&self) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.env("XDG_RUNTIME_DIR", self.root.path().join("runtime"))
            .env_remove("SHUX_SOCKET")
            .current_dir(self.root.path());
        cmd
    }

    fn start_shell(&self) -> String {
        let created = self
            .shux()
            .args([
                "session",
                "create",
                "ws",
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
            .args([
                "pane",
                "wait-for",
                "-s",
                "ws",
                "--text",
                "wait-status-prompt",
            ])
            .args(["--timeout-ms", "30000"])
            .status()
            .expect("wait-for");
        assert!(ready.success(), "the shell never drew its prompt");
        let panes = self
            .shux()
            .args(["--format", "json", "pane", "list", "-s", "ws"])
            .output()
            .expect("pane list");
        let panes: serde_json::Value = serde_json::from_slice(&panes.stdout).expect("json");
        panes[0]["id"].as_str().expect("pane id").to_string()
    }

    /// A command in the pane that reports `done` for `id`, as an agent's would.
    fn report_done(&self, id: &str) {
        let out = self
            .shux()
            .args([
                "--format",
                "json",
                "pane",
                "run",
                "-s",
                "ws",
                "--timeout",
                "30",
                "-c",
            ])
            .arg(format!(r"printf '\033]7501;state=done:id={id}\033\\'"))
            .output()
            .expect("spawn shux");
        assert!(
            out.status.success(),
            "pane run failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn rpc(&self, method: &str, params: serde_json::Value) -> Command {
        let mut cmd = self.shux();
        cmd.args(["--format", "json", "rpc", "call", method, "--params"])
            .arg(params.to_string());
        cmd
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self.shux().args(["daemon", "stop"]).output();
    }
}

fn result(out: &Output) -> serde_json::Value {
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "not json ({e}): {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    v.get("result").cloned().unwrap_or(v)
}

#[test]
fn a_wait_names_its_report_and_never_matches_an_earlier_one() {
    let env = Env::new();
    let pane = env.start_shell();
    env.report_done("job1");
    let first = result(
        &env.rpc(
            "pane.wait_status",
            serde_json::json!({"pane_id": pane, "state": "done", "id": "job1", "timeout_ms": 20000}),
        )
        .output()
        .expect("spawn shux"),
    );
    assert_eq!(first["outcome"], "matched", "{first}");

    // A fresh id: job1's `done` is still in the pane and must not satisfy it.
    let fresh = result(
        &env.rpc(
            "pane.wait_status",
            serde_json::json!({"pane_id": pane, "state": "done", "id": "job2", "timeout_ms": 1500}),
        )
        .output()
        .expect("spawn shux"),
    );
    assert_eq!(
        fresh["outcome"], "timeout",
        "a wait for job2 matched the earlier report: {fresh}"
    );

    // A reused id, waited on before its command runs, with the floor read
    // first: only the new report, identical to the old one, may satisfy it.
    let status = result(
        &env.rpc("pane.status", serde_json::json!({"pane_id": pane}))
            .output()
            .expect("spawn shux"),
    );
    let as_of = status["as_of"]
        .as_u64()
        .unwrap_or_else(|| panic!("no as_of: {status}"));
    let waiter = env
        .rpc(
            "pane.wait_status",
            serde_json::json!({
                "pane_id": pane, "state": "done", "id": "job1",
                "after": as_of, "timeout_ms": 20000,
            }),
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn shux");
    env.report_done("job1");
    let rerun = result(&waiter.wait_with_output().expect("waiter"));
    assert_eq!(rerun["outcome"], "matched", "{rerun}");
    let stamp = rerun["record"]["reported_generation"].as_u64();
    assert!(
        stamp.is_some_and(|g| g > as_of),
        "the wait matched a report from before its floor {as_of}: {rerun}"
    );
    assert_eq!(rerun["record"]["id"], "job1", "{rerun}");

    // The CLI sends what it was given: `--id` alone must not be refused for a
    // `match` the user never passed.
    let cli = |id: &str| {
        env.shux()
            .args(["pane", "wait-status", &pane, "--state", "done", "--id", id])
            .args(["--timeout", "1s"])
            .output()
            .expect("spawn shux")
    };
    let seen = cli("job1");
    assert_eq!(
        seen.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&seen.stderr)
    );
    assert_eq!(
        cli("job2").status.code(),
        Some(1),
        "a fresh id must time out"
    );

    let bad = env
        .rpc(
            "pane.wait_status",
            serde_json::json!({"pane_id": pane, "state": "done", "id": "", "timeout_ms": 100}),
        )
        .output()
        .expect("spawn shux");
    assert!(
        !bad.status.success(),
        "an empty id was accepted: {}",
        String::from_utf8_lossy(&bad.stdout)
    );
}
