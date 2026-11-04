//! `pane wait-command`: a command typed into a real shell that sources
//! `shux shell-integration` is waited for by its own end, with its exit code,
//! and no marker of the caller's (issue #261).

mod pane_e2e;

use pane_e2e::Env;
use std::process::{Command, Output, Stdio};

const PROMPT: &str = "wait-command-prompt";

/// A bash that has PS0 (4.4+), if one is installed.
fn modern_bash() -> Option<String> {
    [
        "bash",
        "/opt/homebrew/bin/bash",
        "/usr/local/bin/bash",
        "/bin/bash",
    ]
    .into_iter()
    .find(|b| {
        Command::new(b)
            .args(["-c", "((BASH_VERSINFO[0] * 100 + BASH_VERSINFO[1] >= 404))"])
            .status()
            .is_ok_and(|s| s.success())
    })
    .map(str::to_string)
}

fn has(shell: &str) -> bool {
    Command::new(shell)
        .args(["-c", "true"])
        .status()
        .is_ok_and(|s| s.success())
}

/// A session `ws` running `shell` with an rc that sets a known prompt and,
/// when `integrate`, sources the shell integration. Returns the pane id.
fn start(env: &Env, rc: &tempfile::TempDir, shell: &str, integrate: bool) -> String {
    let bin = env!("CARGO_BIN_EXE_shux");
    let zsh = shell.ends_with("zsh");
    let hook = if integrate {
        format!(
            "eval \"$('{bin}' shell-integration {})\"\n",
            if zsh { "zsh" } else { "bash" }
        )
    } else {
        String::new()
    };
    let rcfile = rc.path().join("rc");
    let prompt = if zsh {
        format!("PROMPT='{PROMPT} %# '\n")
    } else {
        format!("PS1='{PROMPT} $ '\n")
    };
    // zsh reads /etc/zshenv however it is started, and a broken one sets
    // PATH: the rc restores this process's own.
    let path = std::env::var("PATH").expect("PATH");
    std::fs::write(&rcfile, format!("export PATH='{path}'\n{prompt}{hook}")).expect("rcfile");
    let mut create = env.shux();
    create.args(["session", "create", "ws", "-d", "--"]);
    if zsh {
        // `-f`: none of the user's rc files; the rc is sourced as the first
        // command instead.
        create.args([shell, "-f", "-i"]);
    } else {
        create
            .args([shell, "--noprofile", "--rcfile"])
            .arg(&rcfile)
            .arg("-i");
    }
    let created = create.output().expect("spawn shux");
    assert!(
        created.status.success(),
        "session create failed: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let panes: serde_json::Value =
        serde_json::from_str(&env.ok(&["--format", "json", "pane", "list", "-s", "ws"]))
            .expect("json");
    let pane = panes[0]["id"].as_str().expect("pane id").to_string();
    if zsh {
        env.wait_for("ws", &pane, "%");
        type_line(env, &format!("source '{}'", rcfile.display()));
    }
    env.wait_for("ws", &pane, PROMPT);
    pane
}

fn type_line(env: &Env, line: &str) {
    env.ok(&["pane", "send-keys", "-s", "ws", "--text", line]);
    env.ok(&["pane", "send-keys", "-s", "ws", "--data", "DQ=="]);
}

fn as_of(env: &Env, pane: &str) -> u64 {
    let status: serde_json::Value =
        serde_json::from_str(&env.ok(&["--format", "json", "pane", "status", pane])).expect("json");
    status["result"]["as_of"]
        .as_u64()
        .unwrap_or_else(|| panic!("no as_of: {status}"))
}

fn wait(env: &Env, pane: &str, after: u64, timeout: &str) -> Command {
    let mut cmd = env.shux();
    cmd.args(["--format", "json", "pane", "wait-command", pane, "--after"])
        .arg(after.to_string())
        .args(["--timeout", timeout]);
    cmd
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

/// Every command's own exit code, in order, with no marker printed: waited
/// one at a time, typed back to back, and with the wait started first.
fn commands_report_their_own_ends(shell: &str) {
    let env = Env::new(80, 24);
    let rc = tempfile::tempdir().expect("rc dir");
    let pane = start(&env, &rc, shell, true);

    let floor = as_of(&env, &pane);
    type_line(&env, "false");
    let out = wait(&env, &pane, floor, "20s")
        .output()
        .expect("spawn shux");
    let first = result(&out);
    assert_eq!(first["outcome"], "completed", "{shell}: {first}");
    assert_eq!(first["exit_code"], 1, "{shell}: {first}");
    assert_eq!(
        out.status.code(),
        Some(0),
        "{shell}: a failed command is a completed wait"
    );

    // Typed back to back: chaining from each `started` loses none of them.
    type_line(&env, "(exit 7)");
    type_line(&env, "true");
    let mut after = first["started"].as_u64().expect("started");
    let mut codes = Vec::new();
    for _ in 0..2 {
        let r = result(
            &wait(&env, &pane, after, "20s")
                .output()
                .expect("spawn shux"),
        );
        assert_eq!(r["outcome"], "completed", "{shell}: {r}");
        codes.push(r["exit_code"].clone());
        after = r["started"].as_u64().expect("started");
    }
    assert_eq!(
        codes,
        [7, 0],
        "{shell}: a back-to-back command was lost or reordered"
    );

    // The wait first, then the command: only the new command answers it.
    let waiter = wait(&env, &pane, after, "20s")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn shux");
    type_line(&env, "sh -c 'exit 3'");
    let r = result(&waiter.wait_with_output().expect("waiter"));
    assert_eq!(
        (r["outcome"].as_str(), r["exit_code"].as_i64()),
        (Some("completed"), Some(3)),
        "{shell}: {r}"
    );
}

#[test]
fn bash_commands_report_their_own_ends() {
    let bash = modern_bash().expect("no bash 4.4+ installed: the integration cannot be tested");
    commands_report_their_own_ends(&bash);
}

#[test]
fn zsh_commands_report_their_own_ends() {
    if !has("zsh") {
        // zsh is optional on Linux; bash covers the protocol there.
        eprintln!("zsh not installed: skipped");
        return;
    }
    commands_report_their_own_ends("zsh");
}

/// A shell that reports nothing: the timeout says so, instead of only
/// timing out.
#[test]
fn a_shell_without_integration_is_reported_as_such() {
    let env = Env::new(80, 24);
    let rc = tempfile::tempdir().expect("rc dir");
    let bash = modern_bash().expect("no bash 4.4+ installed");
    let pane = start(&env, &rc, &bash, false);
    let floor = as_of(&env, &pane);
    type_line(&env, "true");
    let out = wait(&env, &pane, floor, "1500ms")
        .output()
        .expect("spawn shux");
    let r = result(&out);
    assert_eq!(r["outcome"], "timeout", "{r}");
    assert_eq!(r["no_integration"], true, "{r}");
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn the_script_installs_once_and_old_bash_is_told_why_not() {
    let bin = env!("CARGO_BIN_EXE_shux");
    let bash = modern_bash().expect("no bash 4.4+ installed");
    // Sourced twice, PROMPT_COMMAND gains the hook once, ahead of the rest.
    let out = Command::new(&bash)
        .args(["--noprofile", "--norc", "-i", "-c"])
        .arg(format!(
            "PROMPT_COMMAND='echo mine'; eval \"$('{bin}' shell-integration bash)\"; \
             eval \"$('{bin}' shell-integration bash)\"; printf '%s|%s' \"$PROMPT_COMMAND\" \"$PS0\""
        ))
        .output()
        .expect("bash");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "__shux_osc133_precmd; echo mine|\\e]133;C;shux=$((__shux_osc133_ran = 1))\\a",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    if Command::new("/bin/bash")
        .args(["-c", "((BASH_VERSINFO[0] * 100 + BASH_VERSINFO[1] < 404))"])
        .status()
        .is_ok_and(|s| s.success())
    {
        let out = Command::new("/bin/bash")
            .args(["--noprofile", "--norc", "-i", "-c"])
            .arg(format!(
                "eval \"$('{bin}' shell-integration bash)\"; printf '%s' \"${{PS0-}}\""
            ))
            .output()
            .expect("bash");
        assert!(out.stdout.is_empty(), "old bash got hooks it cannot run");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("need bash 4.4 or newer"),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// A shell started inside the integrated one draws its first prompt while
/// the outer command (`bash`, `sudo -i`, `ssh host`) still runs: that prompt
/// must not report the outer command as ended with a made-up exit 0.
fn a_nested_shell_does_not_report_a_fake_success(shell: &str) {
    let env = Env::new(80, 24);
    let rc = tempfile::tempdir().expect("rc dir");
    let pane = start(&env, &rc, shell, true);
    let floor = as_of(&env, &pane);
    let rcfile = rc.path().join("rc");
    if shell.ends_with("zsh") {
        type_line(&env, &format!("{shell} -f -i"));
        env.wait_for("ws", &pane, "%");
        type_line(&env, &format!("source '{}'", rcfile.display()));
    } else {
        type_line(
            &env,
            &format!("{shell} --noprofile --rcfile '{}' -i", rcfile.display()),
        );
    }
    let r = result(
        &wait(&env, &pane, floor, "20s")
            .output()
            .expect("spawn shux"),
    );
    assert_ne!(
        (r["exit_code"].as_i64(), r["ended_by"].as_str()),
        (Some(0), Some("end")),
        "{shell}: the inner shell's first prompt reported the outer command as a success: {r}"
    );
    type_line(&env, "exit");
}

#[test]
fn a_nested_bash_does_not_report_a_fake_success() {
    let bash = modern_bash().expect("no bash 4.4+ installed: the integration cannot be tested");
    a_nested_shell_does_not_report_a_fake_success(&bash);
}

#[test]
fn a_nested_zsh_does_not_report_a_fake_success() {
    if !has("zsh") {
        eprintln!("zsh not installed: skipped");
        return;
    }
    a_nested_shell_does_not_report_a_fake_success("zsh");
}

/// With errexit on in the user's shell, a failed command must not take the
/// shell with it through the hooks.
#[test]
fn the_hooks_do_not_end_a_shell_running_with_errexit() {
    let bash = modern_bash().expect("no bash 4.4+ installed");
    for shell in [bash.as_str(), "zsh"] {
        if !has(shell) {
            continue;
        }
        let env = Env::new(80, 24);
        let rc = tempfile::tempdir().expect("rc dir");
        let pane = start(&env, &rc, shell, true);
        let errexit = if shell.ends_with("zsh") {
            "setopt ERR_EXIT"
        } else {
            "set -e"
        };
        type_line(&env, errexit);
        type_line(&env, "[[ -f /nonexistent ]] && echo never");
        type_line(&env, "echo AL\"\"IVE");
        let alive = env.run(&[
            "pane",
            "wait-for",
            "-s",
            "ws",
            "-p",
            &pane,
            "--text",
            "ALIVE",
            "--timeout-ms",
            "10000",
        ]);
        assert!(
            alive.status.success(),
            "{shell}: the hooks ended a shell running with errexit"
        );
    }
}

/// A `pane run` line is one command to the shell: its own exit code, not the
/// status of the wrapper's last statement.
#[test]
fn a_pane_run_line_reports_the_commands_own_exit_code() {
    let env = Env::new(80, 24);
    let rc = tempfile::tempdir().expect("rc dir");
    let bash = modern_bash().expect("no bash 4.4+ installed");
    let pane = start(&env, &rc, &bash, true);
    let floor = as_of(&env, &pane);
    let ran: serde_json::Value = serde_json::from_str(&env.ok(&[
        "--format",
        "json",
        "pane",
        "run",
        "-s",
        "ws",
        "--timeout",
        "20",
        "-c",
        "(exit 4)",
    ]))
    .expect("json");
    assert_eq!(ran["exit_code"], 4, "{ran}");
    let r = result(
        &wait(&env, &pane, floor, "20s")
            .output()
            .expect("spawn shux"),
    );
    assert_eq!(
        r["exit_code"], 4,
        "the shell reported the wrapper's status, not the command's: {r}"
    );
}

/// With allexport on, everything the hooks set is exported. A shell started
/// inside must neither inherit "this shell started a command" (its first
/// prompt would fake the parent's success) nor stack a second set of hooks.
#[test]
fn a_child_of_an_allexport_shell_installs_cleanly() {
    let bin = env!("CARGO_BIN_EXE_shux");
    let bash = modern_bash().expect("no bash 4.4+ installed");
    let child = r#"eval "$("$SHUX_BIN" shell-integration bash)"; printf "%s|%s|%s" "${__shux_osc133_ran-}" "$PS0" "${PROMPT_COMMAND[*]}""#;
    let out = Command::new(&bash)
        .env("SHUX_BIN", bin)
        .env("CHILD_BASH", &bash)
        .env("CHILD", child)
        .args(["--noprofile", "--norc", "-i", "-c"])
        .arg(r#"set -a; eval "$("$SHUX_BIN" shell-integration bash)"; __shux_osc133_ran=1; "$CHILD_BASH" --noprofile --norc -i -c "$CHILD""#)
        .output()
        .expect("bash");
    let got = String::from_utf8_lossy(&out.stdout);
    let fields: Vec<&str> = got.split('|').collect();
    assert_eq!(
        fields.len(),
        3,
        "{got:?} {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fields[0], "",
        "the child inherited its parent's started-command flag"
    );
    assert_eq!(
        fields[1].matches("133;C").count(),
        1,
        "PS0 got a second start: {got:?}"
    );
    assert_eq!(
        fields[2].matches("__shux_osc133_precmd").count(),
        1,
        "PROMPT_COMMAND got a second hook: {got:?}"
    );
}

/// With `promptvars` off, PS0 is never expanded, so the hook cannot tell it
/// started the command: it must still report the exit code.
#[test]
fn a_bash_with_promptvars_off_still_reports_exit_codes() {
    let env = Env::new(80, 24);
    let rc = tempfile::tempdir().expect("rc dir");
    let bash = modern_bash().expect("no bash 4.4+ installed");
    let pane = start(&env, &rc, &bash, true);
    let floor = as_of(&env, &pane);
    type_line(&env, "shopt -u promptvars");
    let shopt = result(
        &wait(&env, &pane, floor, "20s")
            .output()
            .expect("spawn shux"),
    );
    assert_eq!(shopt["outcome"], "completed", "{shopt}");
    type_line(&env, "(exit 7)");
    let after = shopt["started"].as_u64().expect("started");
    let r = result(
        &wait(&env, &pane, after, "20s")
            .output()
            .expect("spawn shux"),
    );
    assert_eq!(r["exit_code"], 7, "{r}");
}
