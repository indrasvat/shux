//! `pane wait-status` waits for one named report, not for whatever `done` a
//! previous command left behind (issue #260). Driven through the real daemon,
//! in an interactive bash whose commands report over OSC 7501.

mod pane_e2e;

use pane_e2e::Env;
use std::process::{Command, Output, Stdio};

/// An interactive bash whose commands can report, drawn and ready. Returns
/// the pane id; `rc` must outlive the session.
fn start_shell(env: &Env, rc: &tempfile::TempDir) -> String {
    let rcfile = rc.path().join("rc");
    std::fs::write(&rcfile, "PS1='wait-status-prompt $ '\n").expect("rcfile");
    let created = env
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
        .arg(&rcfile)
        .arg("-i")
        .output()
        .expect("spawn shux");
    assert!(created.status.success(), "session create failed");
    env.ok(&[
        "pane",
        "wait-for",
        "-s",
        "ws",
        "--text",
        "wait-status-prompt",
        "--timeout-ms",
        "30000",
    ]);
    let panes: serde_json::Value =
        serde_json::from_str(&env.ok(&["--format", "json", "pane", "list", "-s", "ws"]))
            .expect("json");
    panes[0]["id"].as_str().expect("pane id").to_string()
}

/// Run `cmd` in the pane, to completion.
fn emit(env: &Env, cmd: &str) {
    env.ok(&[
        "--format",
        "json",
        "pane",
        "run",
        "-s",
        "ws",
        "--timeout",
        "30",
        "-c",
        cmd,
    ]);
}

/// A command in the pane that reports `done` for `id`, as an agent's would.
fn report_done(env: &Env, id: &str) {
    emit(
        env,
        &format!(r"printf '\033]7501;state=done:id={id}\033\\'"),
    );
}

fn rpc(env: &Env, method: &str, params: serde_json::Value) -> Command {
    let mut cmd = env.shux();
    cmd.args(["--format", "json", "rpc", "call", method, "--params"])
        .arg(params.to_string());
    cmd
}

fn as_of(env: &Env, pane: &str) -> u64 {
    let status = result(
        &rpc(env, "pane.status", serde_json::json!({"pane_id": pane}))
            .output()
            .expect("spawn shux"),
    );
    status["as_of"]
        .as_u64()
        .unwrap_or_else(|| panic!("no as_of: {status}"))
}

/// A wait started now, in the background, so the command can run after it.
fn waiter(env: &Env, params: serde_json::Value) -> std::process::Child {
    rpc(env, "pane.wait_status", params)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn shux")
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
    let env = Env::new(80, 24);
    let rc = tempfile::tempdir().expect("rc dir");
    let pane = start_shell(&env, &rc);
    report_done(&env, "job1");
    let first = result(
        &rpc(
            &env,
            "pane.wait_status",
            serde_json::json!({"pane_id": pane, "state": "done", "id": "job1", "timeout_ms": 20000}),
        )
        .output()
        .expect("spawn shux"),
    );
    assert_eq!(first["outcome"], "matched", "{first}");

    // A fresh id: job1's `done` is still in the pane and must not satisfy it.
    let fresh = result(
        &rpc(
            &env,
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
    let as_of = as_of(&env, &pane);
    let waiter = waiter(
        &env,
        serde_json::json!({
            "pane_id": pane, "state": "done", "id": "job1",
            "after": as_of, "timeout_ms": 20000,
        }),
    );
    report_done(&env, "job1");
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
}

/// A reported `done` that the record leaves at once (`working`, `idle`) is
/// still the report the wait asked for, for a reused id with a floor and for
/// a fresh one alike.
#[test]
fn a_report_the_record_leaves_at_once_still_counts() {
    let env = Env::new(80, 24);
    let rc = tempfile::tempdir().expect("rc dir");
    let pane = start_shell(&env, &rc);
    report_done(&env, "job");

    let floor = as_of(&env, &pane);
    let reused = waiter(
        &env,
        serde_json::json!({"pane_id": pane, "state": "done", "id": "job", "after": floor, "timeout_ms": 10000}),
    );
    let fresh = waiter(
        &env,
        serde_json::json!({"pane_id": pane, "state": "done", "id": "next", "timeout_ms": 10000}),
    );
    emit(
        &env,
        r"printf '\033]7501;state=done:id=job\033\\\033]7501;state=done:id=next\033\\'; printf '\033]7501;state=working:id=job\033\\\033]7501;state=idle:id=next\033\\'",
    );
    for (name, child) in [("reused", reused), ("fresh", fresh)] {
        let r = result(&child.wait_with_output().expect("waiter"));
        assert_eq!(r["outcome"], "matched", "{name}: the done was lost: {r}");
        assert_eq!(r["reported"]["state"], "done", "{name}: {r}");
    }
}
