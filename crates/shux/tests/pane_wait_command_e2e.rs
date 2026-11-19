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

/// An interactive shell the test runs itself, in a session of its own. One
/// that shares the caller's terminal opens it, and a run of the tests from a
/// terminal then leaves it stopped by SIGTTOU until the test times out.
fn detached(program: &str) -> Command {
    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new(program);
    // SAFETY: `setsid` is async-signal-safe and touches no memory of the
    // parent: what `pre_exec` requires between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            nix::unistd::setsid()
                .map(drop)
                .map_err(std::io::Error::from)
        });
    }
    cmd
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
    start_with(env, rc, shell, integrate, "")
}

/// [`start`], with `before_hook` in the rc ahead of the integration.
fn start_with(
    env: &Env,
    rc: &tempfile::TempDir,
    shell: &str,
    integrate: bool,
    before_hook: &str,
) -> String {
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
    std::fs::write(
        &rcfile,
        format!("export PATH='{path}'\n{prompt}{before_hook}{hook}"),
    )
    .expect("rcfile");
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
    let out = detached(&bash)
        .args(["--noprofile", "--norc", "-i", "-c"])
        .arg(format!(
            "PROMPT_COMMAND='echo mine'; eval \"$('{bin}' shell-integration bash)\"; \
             eval \"$('{bin}' shell-integration bash)\"; printf '%s|%s' \"$PROMPT_COMMAND\" \"$PS0\""
        ))
        .output()
        .expect("bash");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "__shux_osc133_precmd && :; echo mine|\\e]133;C;shux=$((__shux_osc133_ran[0] = $$))\\a",
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
    // No exit code is known: the inner prompt only closes the command.
    assert_eq!(
        (
            r["outcome"].as_str(),
            r["ended_by"].as_str(),
            r["exit_code"].as_i64()
        ),
        (Some("completed"), Some("prompt"), None),
        "{shell}: the inner shell's first prompt reported the outer command as ended: {r}"
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
        type_line(
            &env,
            &format!("printf '{}\\n'; echo AL\"\"IVE", pane_e2e::COLOUR_PROBE),
        );
        // Panics with the screen if the hooks ended the shell.
        env.wait_for("ws", &pane, "ALIVE");
        env.assert_colour_probe(&pane);
    }
}

/// A user's ERR trap fires once for a failing command typed at the prompt,
/// as it does without the integration: the hooks hand the status on, but
/// must not fail a second time doing so.
#[test]
fn a_users_err_trap_fires_once_per_failing_command() {
    let bash = modern_bash().expect("no bash 4.4+ installed");
    for shell in [bash.as_str(), "zsh"] {
        if !has(shell) {
            continue;
        }
        let env = Env::new(80, 24);
        let rc = tempfile::tempdir().expect("rc dir");
        let pane = start(&env, &rc, shell, true);
        // Spelled apart, so the typed line cannot match the count.
        let trap = if shell.ends_with("zsh") {
            "TRAPZERR() { echo ERR-\"TR\"AP; }"
        } else {
            "trap 'echo ERR-\"TR\"AP' ERR"
        };
        type_line(&env, trap);
        type_line(&env, "false");
        type_line(
            &env,
            &format!("printf '{}\\n'; echo DO\"\"NE", pane_e2e::COLOUR_PROBE),
        );
        env.wait_for("ws", &pane, "DONE");
        env.assert_colour_probe(&pane);
        let screen = env.capture("ws", &pane);
        assert_eq!(
            screen.matches("ERR-TRAP").count(),
            1,
            "{shell}: the ERR trap ran more than once for one failure:\n{screen}"
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
    let out = detached(&bash)
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

/// A child of an allexport shell inherits the hook functions but sets its
/// own PROMPT_COMMAND in its rc: sourcing the integration must still hook it.
#[test]
fn an_allexport_child_with_its_own_prompt_command_is_hooked() {
    let bash = modern_bash().expect("no bash 4.4+ installed");
    let out = detached(&bash)
        .env("SHUX_BIN", env!("CARGO_BIN_EXE_shux"))
        .env("CHILD_BASH", &bash)
        .args(["--noprofile", "--norc", "-i", "-c"])
        .arg(r#"set -a; eval "$("$SHUX_BIN" shell-integration bash)"; "$CHILD_BASH" --noprofile --norc -i -c 'PROMPT_COMMAND="history -a"; eval "$("$SHUX_BIN" shell-integration bash)"; printf "%s" "$PROMPT_COMMAND"'; true"#)
        .output()
        .expect("bash");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "__shux_osc133_precmd && :; history -a",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Under allexport bash exports the hook functions too: a child bash that
/// never sources the integration inherits the prompt hook and the "started a
/// command" flag, and its first prompt must not end its parent's command.
#[test]
fn an_allexport_child_that_does_not_source_the_integration_reports_nothing() {
    let bash = modern_bash().expect("no bash 4.4+ installed");
    let out = detached(&bash)
        .env("SHUX_BIN", env!("CARGO_BIN_EXE_shux"))
        .env("CHILD_BASH", &bash)
        .args(["--noprofile", "--norc", "-i", "-c"])
        // `: "${PS0@P}"` expands PS0 as bash does before running a command;
        // `; true` keeps bash from exec'ing the child in its own PID.
        .arg(r#"set -a; eval "$("$SHUX_BIN" shell-integration bash)"; : "${PS0@P}"; "$CHILD_BASH" --noprofile --norc -c 'declare -F __shux_osc133_precmd >/dev/null || echo NOT-INHERITED; __shux_osc133_precmd'; true"#)
        .output()
        .expect("bash");
    let got = String::from_utf8_lossy(&out.stdout);
    assert!(
        !got.contains("NOT-INHERITED"),
        "the premise failed: {got:?}"
    );
    assert!(
        got.contains("133;A") && !got.contains("133;D"),
        "the child's prompt ended its parent's command: {got:?} {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `exec bash` under allexport keeps the PID and inherits the hook: the new
/// shell's first prompt must not report `exec` as ended with exit 0.
#[test]
fn an_allexport_exec_of_bash_reports_no_made_up_end() {
    let bash = modern_bash().expect("no bash 4.4+ installed");
    let out = detached(&bash)
        .env("SHUX_BIN", env!("CARGO_BIN_EXE_shux"))
        .env("CHILD_BASH", &bash)
        .args(["--noprofile", "--norc", "-i", "-c"])
        .arg(r#"set -a; eval "$("$SHUX_BIN" shell-integration bash)"; : "${PS0@P}"; exec "$CHILD_BASH" --noprofile --norc -c '__shux_osc133_precmd'"#)
        .output()
        .expect("bash");
    let got = String::from_utf8_lossy(&out.stdout);
    assert!(
        got.contains("133;A") && !got.contains("133;D"),
        "the exec'd shell's prompt made up an end: {got:?} {}",
        String::from_utf8_lossy(&out.stderr)
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

/// `set -u` before the integration is sourced: an unset PS0,
/// PROMPT_COMMAND or zsh hook array must not stop it installing.
#[test]
fn the_script_installs_under_nounset() {
    let bash = modern_bash().expect("no bash 4.4+ installed");
    let out = detached(&bash)
        .env("SHUX_BIN", env!("CARGO_BIN_EXE_shux"))
        .args(["--noprofile", "--norc", "-i", "-c"])
        .arg(r#"set -u; unset PS0 PROMPT_COMMAND; eval "$("$SHUX_BIN" shell-integration bash)"; printf '%s' "${PROMPT_COMMAND-}""#)
        .output()
        .expect("bash");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("__shux_osc133_precmd"),
        "bash: the hook was not installed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    if !has("zsh") {
        return;
    }
    let out = detached("zsh")
        .env("SHUX_BIN", env!("CARGO_BIN_EXE_shux"))
        .args(["-f", "-i", "-c"])
        .arg(r#"setopt nounset; unset preexec_functions precmd_functions; eval "$("$SHUX_BIN" shell-integration zsh)"; print -r -- "${precmd_functions[*]}""#)
        .output()
        .expect("zsh");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("__shux_osc133_precmd"),
        "zsh: the hook was not installed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ── `pane run` on the shell's own boundaries ─────────────────────────────────

/// Truecolor, indexed and basic: a run whose output loses colour on screen,
/// or keeps escapes in `stdout`, cannot pass.
const COLOUR_PROBE: &str = r"printf '\033[38;2;120;220;180mT\033[38;5;208mI\033[34mB\033[0m\n'";

fn pane_run(
    env: &Env,
    pane: &str,
    command: &str,
    extra: &[&str],
) -> (serde_json::Value, Option<i32>) {
    let mut cmd = env.shux();
    cmd.args([
        "--format", "json", "pane", "run", "-s", "ws", "-p", pane, "-c", command,
    ])
    .args(extra);
    let out = cmd.output().expect("spawn shux");
    (result(&out), out.status.code())
}

fn integrated(shell: &str) -> (Env, tempfile::TempDir, String) {
    let env = Env::new(80, 24);
    let rc = tempfile::tempdir().expect("rc dir");
    let pane = start(&env, &rc, shell, true);
    (env, rc, pane)
}

fn bash() -> String {
    modern_bash().expect("no bash 4.4+ installed: the integration cannot be tested")
}

/// With the integration sourced, only the command and its output reach the
/// screen, and the JSON still carries both. It is the pane's FIRST command:
/// a fresh shell has drawn a prompt and nothing else.
fn run_types_only_the_command(shell: &str) {
    let (env, _rc, pane) = integrated(shell);
    let (r, code) = pane_run(&env, &pane, "echo hi", &[]);
    assert_eq!(
        (r["stdout"].as_str(), r["exit_code"].as_i64()),
        (Some("hi\n"), Some(0)),
        "{shell}: {r}"
    );
    assert_eq!(r["boundaries"], "osc133", "{shell}: {r}");
    assert_eq!(r["ended_by"], "end", "{shell}: {r}");
    assert_eq!(code, Some(0));
    let (r, _) = pane_run(&env, &pane, COLOUR_PROBE, &[]);
    assert_eq!(r["stdout"], "TIB\n", "{shell}: escapes are not output: {r}");
    let screen = env.capture("ws", &pane);
    assert!(
        !screen.contains("SHUX_") && !screen.contains("__shux"),
        "{shell}: a marker reached the screen:\n{screen}"
    );
    assert!(screen.contains("echo hi"), "{shell}:\n{screen}");
    env.assert_colour_probe(&pane);
    let (r, code) = pane_run(&env, &pane, "false", &[]);
    assert_eq!(
        (r["state"].as_str(), r["exit_code"].as_i64(), code),
        (Some("completed"), Some(1), Some(0)),
        "{shell}: {r}"
    );
}

#[test]
fn bash_pane_run_types_only_the_command() {
    run_types_only_the_command(&bash());
}

#[test]
fn zsh_pane_run_types_only_the_command() {
    if !has("zsh") {
        eprintln!("zsh not installed: skipped");
        return;
    }
    run_types_only_the_command("zsh");
}

/// Without the integration, nothing changes but the report of how.
#[test]
fn pane_run_without_integration_uses_markers_and_says_so() {
    let env = Env::new(80, 24);
    let rc = tempfile::tempdir().expect("rc dir");
    let pane = start(&env, &rc, &bash(), false);
    let (r, code) = pane_run(&env, &pane, "echo hi", &[]);
    assert_eq!(
        (r["stdout"].as_str(), r["exit_code"].as_i64(), code),
        (Some("hi\n"), Some(0), Some(0)),
        "{r}"
    );
    assert_eq!(r["boundaries"], "markers", "{r}");
    // Asked for the shell's marks, a shell that sends none is refused, after
    // waiting for a prompt that never says so, with nothing typed.
    let (r, code) = pane_run(
        &env,
        &pane,
        "echo never-typed-275",
        &["--boundaries", "osc133", "--timeout", "1"],
    );
    assert_eq!(code, Some(3), "{r}");
    assert_eq!(r["error"]["data"]["reason"], "no_prompt", "{r}");
    assert!(!env.capture("ws", &pane).contains("never-typed-275"));
}

/// Output longer than the screen and wider than it, exactly as written.
#[test]
fn wait_command_output_is_exactly_what_the_command_wrote() {
    let (env, _rc, pane) = integrated(&bash());
    let floor = as_of(&env, &pane);
    type_line(&env, "seq 1 200; printf '%0300d\\n' 7");
    let out = wait(&env, &pane, floor, "20s")
        .arg("--output")
        .output()
        .expect("spawn shux");
    let r = result(&out);
    let want: String =
        (1..=200).map(|n| format!("{n}\n")).collect::<String>() + &format!("{:0300}\n", 7);
    assert_eq!(r["stdout"].as_str(), Some(want.as_str()), "{r}");
    assert_eq!(r["truncated"], false, "{r}");
    // Without --output the shape is as before.
    let floor = as_of(&env, &pane);
    type_line(&env, "true");
    let r = result(
        &wait(&env, &pane, floor, "20s")
            .output()
            .expect("spawn shux"),
    );
    assert!(r.get("stdout").is_none(), "{r}");
    // `pane run`'s stdout agrees with `wait-command --output` for the same text.
    let (run, _) = pane_run(&env, &pane, "seq 1 200; printf '%0300d\\n' 7", &[]);
    assert_eq!(run["stdout"].as_str(), Some(want.as_str()));
}

/// A pane whose input would not reach its shell's prompt is never typed
/// into, and the refusal names why.
#[test]
fn pane_run_refuses_a_pane_not_at_its_prompt() {
    let (env, _rc, pane) = integrated(&bash());
    let floor = as_of(&env, &pane);
    type_line(
        &env,
        "printf 'read''ing-275\\n'; read -rs secret; echo got-$secret",
    );
    env.wait_for("ws", &pane, "reading-275");
    let (r, code) = pane_run(&env, &pane, "echo leaked-275", &[]);
    assert_eq!(code, Some(3), "{r}");
    assert_eq!(r["error"]["data"]["reason"], "hidden_input", "{r}");
    type_line(&env, "pw");
    let r = result(
        &wait(&env, &pane, floor, "20s")
            .arg("--output")
            .output()
            .expect("spawn shux"),
    );
    assert_eq!(
        r["stdout"], "reading-275\ngot-pw\n",
        "the refused line was typed into the prompt: {r}"
    );

    type_line(&env, "printf 'sleep''ing-275\\n'; sleep 30");
    env.wait_for("ws", &pane, "sleeping-275");
    let (r, code) = pane_run(&env, &pane, "echo leaked-275", &[]);
    assert_eq!(code, Some(3), "{r}");
    assert_eq!(r["error"]["data"]["reason"], "other_program", "{r}");
    assert!(!env.capture("ws", &pane).contains("leaked-275"));
}

/// No integration, so no marks: the foreground process group still tells a
/// running program from the shell.
#[test]
fn pane_run_refuses_a_program_in_front_of_a_shell_without_integration() {
    let env = Env::new(80, 24);
    let rc = tempfile::tempdir().expect("rc dir");
    let pane = start(&env, &rc, &bash(), false);
    type_line(&env, "printf 'sleep''ing-275\\n'; sleep 30");
    env.wait_for("ws", &pane, "sleeping-275");
    let (r, code) = pane_run(&env, &pane, "echo leaked-275", &[]);
    assert_eq!(code, Some(3), "{r}");
    assert_eq!(r["error"]["data"]["reason"], "other_program", "{r}");
    // `markers` types into whatever holds the pane, as asked.
    let (r, _) = pane_run(
        &env,
        &pane,
        "echo typed-anyway",
        &["--boundaries", "markers", "--async"],
    );
    assert!(r["command_id"].is_string(), "{r}");
}

/// The shell drew its next prompt without starting the line: answered at
/// once, with what the shell said, not at the timeout.
fn a_rejected_line_is_not_run(shell: &str, line: &str, said: &str) {
    let (env, _rc, pane) = integrated(shell);
    let (r, code) = pane_run(&env, &pane, line, &["--timeout", "20"]);
    assert_eq!(r["state"], "not_run", "{shell}: {r}");
    assert!(
        r["stdout"].as_str().is_some_and(|s| s.contains(said)),
        "{shell}: {r}"
    );
    assert!(
        r["runtime_ms"].as_u64().is_some_and(|ms| ms < 15_000),
        "{shell}: {r}"
    );
    assert_eq!(code, Some(3));
    // The pane is free again.
    let (r, _) = pane_run(&env, &pane, "echo next", &[]);
    assert_eq!(r["stdout"], "next\n", "{shell}: {r}");
}

#[test]
fn bash_a_syntax_error_is_not_run() {
    a_rejected_line_is_not_run(&bash(), "echo (", "syntax error");
}

#[test]
fn zsh_a_parse_error_is_not_run() {
    if !has("zsh") {
        eprintln!("zsh not installed: skipped");
        return;
    }
    a_rejected_line_is_not_run("zsh", "fi", "parse error");
}

/// A timeout freezes the result; the command's own end still frees the pane,
/// and `wait-command --after <as_of>` still gets it.
#[test]
fn a_timed_out_run_can_be_waited_on_to_its_end() {
    let (env, _rc, pane) = integrated(&bash());
    let (r, code) = pane_run(
        &env,
        &pane,
        "echo early; sleep 3; echo late",
        &["--timeout", "1"],
    );
    assert_eq!(
        (r["state"].as_str(), code),
        (Some("timed_out"), Some(1)),
        "{r}"
    );
    assert_eq!(
        r["stdout"], "early\n",
        "what it wrote before the timeout: {r}"
    );
    let after = r["as_of"]
        .as_u64()
        .unwrap_or_else(|| panic!("no as_of: {r}"));
    let r = result(
        &wait(&env, &pane, after, "20s")
            .arg("--output")
            .output()
            .expect("spawn shux"),
    );
    assert_eq!(r["stdout"], "early\nlate\n", "{r}");
    let (r, _) = pane_run(&env, &pane, "echo free", &[]);
    assert_eq!(r["stdout"], "free\n", "{r}");
}

/// A line submitted and not yet started is ahead of ours: typing now would
/// make ours its input, and its result ours. Reproduced under ble.sh, where
/// the shell accepts a line some time before it reports the start.
#[test]
fn a_line_submitted_and_not_yet_started_is_never_typed_behind() {
    let env = Env::new(80, 24);
    let rc = tempfile::tempdir().expect("rc dir");
    // The shell accepts each line, then waits on this FIFO before it reports
    // the start: the window ble.sh leaves open, held open until released.
    let gate = rc.path().join("gate");
    nix::unistd::mkfifo(&gate, nix::sys::stat::Mode::S_IRWXU).expect("mkfifo");
    let ps0 = format!("PS0='$(read -r _ < {})'\n", gate.display());
    let pane = start_with(&env, &rc, &bash(), true, &ps0);
    let floor = as_of(&env, &pane);
    type_line(&env, "printf 'pass: '; read -rs x; echo got-$x");
    env.wait_for("ws", &pane, "got-$x");
    let (r, code) = pane_run(&env, &pane, "echo leaked-275", &["--timeout", "5"]);
    assert_eq!(code, Some(3), "{r}");
    assert_eq!(r["error"]["data"]["reason"], "input_pending", "{r}");
    std::fs::write(&gate, "go\n").expect("release the line");
    type_line(&env, "pw");
    let r = result(
        &wait(&env, &pane, floor, "20s")
            .arg("--output")
            .output()
            .expect("spawn shux"),
    );
    assert_eq!(
        r["stdout"], "pass: got-pw\n",
        "the refused line reached the shell: {r}"
    );
}

/// A control character in the line would be typed as a key: refused, in
/// every framing, before anything is typed.
#[test]
fn a_control_character_is_refused_before_anything_is_typed() {
    let (env, _rc, pane) = integrated(&bash());
    for boundaries in ["auto", "markers"] {
        let (r, code) = pane_run(&env, &pane, "echo a\tb", &["--boundaries", boundaries]);
        assert_eq!(code, Some(2), "{boundaries}: {r}");
        assert_eq!(r["error"]["data"]["char"], "U+0009", "{boundaries}: {r}");
    }
}

/// One rule for every pane wait: up to an hour, and past it the same refusal.
#[test]
fn every_wait_refuses_past_an_hour_the_same_way() {
    let (env, _rc, pane) = integrated(&bash());
    let over = "3600001";
    let outs = [
        env.run(&[
            "--format",
            "json",
            "pane",
            "wait-for",
            "-s",
            "ws",
            "-p",
            &pane,
            "-t",
            "x",
            "--timeout-ms",
            over,
        ]),
        env.run(&[
            "--format",
            "json",
            "pane",
            "wait-command",
            &pane,
            "--timeout",
            "3600001ms",
        ]),
        env.run(&[
            "--format",
            "json",
            "pane",
            "wait-status",
            &pane,
            "--state",
            "done",
            "--timeout",
            "3600001ms",
        ]),
    ];
    let errors: Vec<serde_json::Value> = outs.iter().map(|o| result(o)["error"].clone()).collect();
    for (o, e) in outs.iter().zip(&errors) {
        assert_eq!(o.status.code(), Some(2), "{e}");
        assert_eq!(e["code"], -32602, "{e}");
        assert_eq!(e["message"], errors[0]["message"], "{errors:?}");
    }
    // An hour is accepted: the wait starts, and its needle ends it.
    let mut wait_for = env.shux();
    wait_for.args([
        "pane",
        "wait-for",
        "-s",
        "ws",
        "-p",
        &pane,
        "-t",
        "accepted-275",
        "--timeout-ms",
        "3600000",
    ]);
    let child = wait_for.stdout(Stdio::null()).spawn().expect("spawn shux");
    type_line(&env, "echo accepted-$((270 + 5))");
    let out = child.wait_with_output().expect("wait-for");
    assert!(out.status.success());
}

/// A FIFO the shell waits on in PS0, after accepting a line and before it
/// reports the start: the window ble.sh leaves open, held until released.
fn gated_bash(env: &Env, rc: &tempfile::TempDir) -> (String, std::path::PathBuf) {
    let gate = rc.path().join("gate");
    nix::unistd::mkfifo(&gate, nix::sys::stat::Mode::S_IRWXU).expect("mkfifo");
    let ps0 = format!("PS0='$(read -r _ < {})'\n", gate.display());
    (start_with(env, rc, &bash(), true, &ps0), gate)
}

/// A line typed ahead while a command ran is the shell's next command. Held
/// after the shell accepted it and before it starts, ours is not typed (it
/// would be echoed into that line and taken for it); once it has run, ours is.
#[test]
fn a_command_typed_ahead_is_never_taken_for_ours() {
    let env = Env::new(80, 24);
    let rc = tempfile::tempdir().expect("rc dir");
    let (pane, gate) = gated_bash(&env, &rc);
    // sleep holds the terminal, so the line typed ahead waits for the shell.
    let floor = as_of(&env, &pane);
    type_line(&env, "sleep 1");
    std::fs::write(&gate, "go\n").expect("release sleep");
    type_line(&env, "echo AHEAD-$((2 + 2))");
    let r = result(
        &wait(&env, &pane, floor, "20s")
            .output()
            .expect("spawn shux"),
    );
    assert_eq!(r["outcome"], "completed", "{r}");
    // The shell reads the line typed ahead at its prompt, accepts it, and
    // waits at the gate: the cursor is at the start of the next row.
    let accepted = env.run(&[
        "pane",
        "wait-for",
        "-s",
        "ws",
        "-p",
        &pane,
        "--regex",
        &format!(r"{PROMPT} \$ echo AHEAD-\$\(\(2 \+ 2\)\)\n"),
        "--timeout-ms",
        "60000",
    ]);
    assert!(accepted.status.success(), "{}", env.capture("ws", &pane));
    let (r, code) = pane_run(&env, &pane, "echo OURS-$((3 + 3))", &["--timeout", "2"]);
    assert_eq!(
        code,
        Some(3),
        "typed behind a line that had not started: {r}"
    );
    assert_eq!(r["error"]["data"]["reason"], "prompt_not_ready", "{r}");
    assert!(
        !env.capture("ws", &pane).contains("OURS-"),
        "the refused line reached the pane"
    );
    std::fs::write(&gate, "go\n").expect("release the line typed ahead");
    env.wait_for("ws", &pane, "AHEAD-4");
    let mut run = env.shux();
    run.args([
        "--format",
        "json",
        "pane",
        "run",
        "-s",
        "ws",
        "-p",
        &pane,
        "-c",
        "echo OURS-$((3 + 3))",
        "--timeout",
        "20",
    ]);
    let child = run.stdout(Stdio::piped()).spawn().expect("spawn shux");
    std::fs::write(&gate, "go\n").expect("release ours");
    let r = result(&child.wait_with_output().expect("pane run"));
    assert_eq!(r["stdout"], "OURS-6\n", "{r}");
}

/// Keys sent to the prompt that submitted nothing: the line editor holds
/// text (and the cursor may be anywhere in it), so nothing is typed until
/// it is cleared.
#[test]
fn a_prompt_with_unsubmitted_keys_is_refused_until_cleared() {
    let (env, _rc, pane) = integrated(&bash());
    env.ok(&["pane", "send-keys", "-s", "ws", "--text", "true; XYZ"]);
    env.ok(&["pane", "send-keys", "-s", "ws", "--data", "G1tEG1tE"]); // Left, Left
    env.wait_for("ws", &pane, "XYZ");
    let (r, code) = pane_run(&env, &pane, "echo CLEAN", &[]);
    assert_eq!(code, Some(3), "{r}");
    assert_eq!(r["error"]["data"]["reason"], "input_present", "{r}");
    env.ok(&["pane", "send-keys", "-s", "ws", "--data", "Aw=="]); // C-c: a fresh prompt
    // The prompt's text follows its mark: once it shows, the mark is read.
    let fresh = env.run(&[
        "pane",
        "wait-for",
        "-s",
        "ws",
        "-p",
        &pane,
        "--regex",
        &format!(r"\^C\s*\n{PROMPT}"),
        "--timeout-ms",
        "60000",
    ]);
    assert!(fresh.status.success(), "{}", env.capture("ws", &pane));
    let (r, code) = pane_run(&env, &pane, "echo CLEAN", &[]);
    assert_eq!(
        (r["stdout"].as_str(), code),
        (Some("CLEAN\n"), Some(0)),
        "{r}"
    );
}

/// zsh's mark is not output, whatever PROMPT_EOL_MARK is; output that only
/// ends like one is.
#[test]
fn zsh_end_of_line_marks_never_reach_stdout() {
    if !has("zsh") {
        eprintln!("zsh not installed: skipped");
        return;
    }
    for mark in ["", "'<EOL>'"] {
        let env = Env::new(80, 24);
        let rc = tempfile::tempdir().expect("rc dir");
        let pane = start_with(&env, &rc, "zsh", true, &format!("PROMPT_EOL_MARK={mark}\n"));
        for (cmd, want) in [
            ("printf '50%%'", "50%"),
            ("printf abc", "abc"),
            ("echo ok", "ok\n"),
        ] {
            let (r, _) = pane_run(&env, &pane, cmd, &[]);
            assert_eq!(r["stdout"], want, "PROMPT_EOL_MARK={mark}: {r}");
        }
    }
}

#[test]
fn output_that_ends_like_a_mark_is_kept_whole() {
    let (env, _rc, pane) = integrated(&bash());
    let (r, _) = pane_run(&env, &pane, r"printf 'Loading 100%%\r \r'", &[]);
    assert_eq!(r["stdout"], "Loading 100%\r \r", "{r}");
}

/// A program that turns echo off without reading is running, not waiting
/// on a password; a timeout says whether the terminal hid input.
#[test]
fn echo_off_is_not_taken_for_a_password_prompt() {
    let (env, _rc, pane) = integrated(&bash());
    let (r, code) = pane_run(
        &env,
        &pane,
        "stty -echo; sleep 2; stty echo; echo done",
        &["--timeout", "20"],
    );
    assert_eq!(
        (r["state"].as_str(), r["stdout"].as_str(), code),
        (Some("completed"), Some("done\n"), Some(0)),
        "{r}"
    );
    let (r, code) = pane_run(
        &env,
        &pane,
        "printf 'pass: '; read -rs x; echo got-$x",
        &["--timeout", "2"],
    );
    assert_eq!(
        (r["state"].as_str(), r["hidden_input"].as_bool(), code),
        (Some("timed_out"), Some(true), Some(1)),
        "{r}"
    );
    assert_eq!(r["stdout"], "pass: ", "{r}");
    let after = r["as_of"]
        .as_u64()
        .unwrap_or_else(|| panic!("no as_of: {r}"));
    type_line(&env, "pw");
    let r = result(
        &wait(&env, &pane, after, "20s")
            .arg("--output")
            .output()
            .expect("spawn shux"),
    );
    assert_eq!(r["stdout"], "pass: got-pw\n", "{r}");
}

/// A pane held by a timed-out command is refused naming it; cancelling an
/// ended command reports its end and sends nothing.
#[test]
fn a_held_pane_names_its_holder_and_an_ended_command_cancels_to_its_state() {
    let (env, _rc, pane) = integrated(&bash());
    let (held, _) = pane_run(&env, &pane, "sleep 3", &["--timeout", "1"]);
    assert_eq!(held["state"], "timed_out", "{held}");
    let (r, code) = pane_run(&env, &pane, "echo second", &[]);
    assert_eq!(code, Some(3), "{r}");
    assert_eq!(
        r["error"]["data"]["running_command_id"], held["command_id"],
        "{r}"
    );
    assert_eq!(r["error"]["data"]["reason"], "pane_held", "{r}");
    // The CLI verbs: status of the holder, then cancel of an ended command.
    let id = held["command_id"].as_str().expect("command id").to_string();
    let status = result(&env.run(&["--format", "json", "pane", "command-status", &id]));
    assert_eq!(status["state"], "timed_out", "{status}");
    let after = held["as_of"]
        .as_u64()
        .unwrap_or_else(|| panic!("no as_of: {held}"));
    let ended = result(
        &wait(&env, &pane, after, "20s")
            .output()
            .expect("spawn shux"),
    );
    assert_eq!(ended["outcome"], "completed", "{ended}");
    let (done, _) = pane_run(&env, &pane, "true", &["--timeout", "20"]);
    assert_eq!(done["state"], "completed", "{done}");
    let done_id = done["command_id"].as_str().expect("command id").to_string();
    let out = env.run(&["--format", "json", "pane", "command-cancel", &done_id]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(result(&out)["state"], "completed");
    let out = env.run(&[
        "--format",
        "json",
        "pane",
        "command-status",
        "00000000-0000-4000-8000-000000000000",
    ]);
    assert_eq!(out.status.code(), Some(2), "no such command");
}

/// Bad targets and bad requests: exit 2 with a JSON error, as every wait.
#[test]
fn bad_targets_and_requests_exit_2_with_a_json_error() {
    let (env, _rc, pane) = integrated(&bash());
    for args in [
        vec!["pane", "run", "-s", "nosuch-275", "-c", "echo x"],
        vec!["pane", "wait-for", "-s", "nosuch-275", "-t", "x"],
        vec![
            "pane",
            "run",
            "-s",
            "ws",
            "-p",
            &pane,
            "-c",
            "echo x",
            "--timeout",
            "3601",
        ],
        vec![
            "pane",
            "run",
            "-s",
            "ws",
            "-p",
            &pane,
            "-c",
            "echo a\necho b",
        ],
    ] {
        let mut full = vec!["--format", "json"];
        full.extend(args.iter().copied());
        let out = env.run(&full);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let r = result(&out);
        assert!(r["error"]["code"].is_i64(), "{args:?}: {r}");
        if args.contains(&"3601") {
            // The waits' own words for their cap.
            assert_eq!(
                r["error"]["message"], "timeout must be at most 3600 (1h), in whole seconds",
                "{r}"
            );
        }
    }
}

/// A pane whose program is not a shell, and reports no prompt, is not typed
/// into unless markers are asked for.
#[test]
fn a_pane_that_is_not_a_shell_is_refused() {
    let env = Env::new(80, 24);
    let created = env.run(&["session", "create", "ws", "-d", "--", "python3", "-q"]);
    assert!(created.status.success());
    let panes: serde_json::Value =
        serde_json::from_str(&env.ok(&["--format", "json", "pane", "list", "-s", "ws"]))
            .expect("json");
    let pane = panes[0]["id"].as_str().expect("pane id").to_string();
    env.wait_for("ws", &pane, ">>>");
    let (r, code) = pane_run(&env, &pane, "print(1)", &[]);
    assert_eq!(code, Some(3), "{r}");
    assert_eq!(r["error"]["data"]["reason"], "not_a_shell", "{r}");
}

/// A shell started through a wrapper (`env`, `bash -c 'exec bash'`) is still
/// a shell: without the integration it gets markers, as before.
#[test]
fn a_wrapped_shell_without_integration_uses_markers() {
    let bash = bash();
    let rc = tempfile::tempdir().expect("rc dir");
    let rcfile = rc.path().join("rc");
    std::fs::write(&rcfile, format!("PS1='{PROMPT} $ '\n")).expect("rcfile");
    let rc = rcfile.display().to_string();
    let exec = format!("exec '{bash}' --noprofile --rcfile '{rc}' -i");
    let wrapped: [Vec<&str>; 2] = [
        vec!["env", "QA=1", &bash, "--noprofile", "--rcfile", &rc, "-i"],
        vec![&bash, "-c", &exec],
    ];
    for argv in wrapped {
        let env = Env::new(80, 24);
        let mut create = vec!["session", "create", "ws", "-d", "--"];
        create.extend(&argv);
        assert!(env.run(&create).status.success(), "{argv:?}");
        let panes: serde_json::Value =
            serde_json::from_str(&env.ok(&["--format", "json", "pane", "list", "-s", "ws"]))
                .expect("json");
        let pane = panes[0]["id"].as_str().expect("pane id").to_string();
        env.wait_for("ws", &pane, PROMPT);
        let (r, code) = pane_run(&env, &pane, "echo hi", &[]);
        assert_eq!(
            (r["stdout"].as_str(), r["boundaries"].as_str(), code),
            (Some("hi\n"), Some("markers"), Some(0)),
            "{argv:?}: {r}"
        );
    }
}

/// A shux call from inside a pane never types into that same pane: it would
/// type into the caller and wait on itself.
#[test]
fn a_run_into_the_callers_own_pane_is_refused() {
    let (env, _rc, pane) = integrated(&bash());
    let mut cmd = env.shux();
    cmd.env("SHUX_PANE_ID", &pane).args([
        "--format", "json", "pane", "run", "-s", "ws", "-c", "echo x",
    ]);
    let out = cmd.output().expect("spawn shux");
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(result(&out)["error"]["data"]["reason"], "own_pane");
    // Every pane's program knows its pane.
    let (r, _) = pane_run(&env, &pane, "echo $SHUX_PANE_ID", &["--timeout", "20s"]);
    assert_eq!(
        r["stdout"].as_str(),
        Some(format!("{pane}\n").as_str()),
        "{r}"
    );
}

/// A hook that reads input after the shell accepted a line: the terminal is
/// in line mode, so nothing is typed (it would be the hook's input, echoed
/// into that line); the line's own command runs as its own.
#[test]
fn a_hook_reading_input_after_an_accepted_line_is_never_typed_into() {
    let (env, _rc, pane) = integrated(&bash());
    type_line(
        &env,
        "sleep 2; PS0='$(printf \"HO\"\"LD>\" >&2; read -r gate)'\"$PS0\"",
    );
    type_line(&env, "printf OTHER");
    let held = env.run(&[
        "pane",
        "wait-for",
        "-s",
        "ws",
        "-p",
        &pane,
        "--text",
        "HOLD>",
        "--timeout-ms",
        "60000",
    ]);
    assert!(held.status.success(), "{}", env.capture("ws", &pane));
    let (r, code) = pane_run(&env, &pane, "echo OURS", &["--timeout", "2"]);
    assert_eq!(
        code,
        Some(3),
        "another command's result came back as ours: {r}"
    );
    assert_eq!(r["error"]["data"]["reason"], "prompt_not_ready", "{r}");
    assert!(
        !env.capture("ws", &pane).contains("OURS"),
        "the refused line reached the hook"
    );
}

/// A command holding a combining character is found, and its pane freed.
#[test]
fn a_command_with_a_combining_character_is_found() {
    let (env, _rc, pane) = integrated(&bash());
    let (r, code) = pane_run(&env, &pane, "printf 'cafe\u{301}\\n'", &["--timeout", "10"]);
    assert_eq!(
        (r["state"].as_str(), r["stdout"].as_str(), code),
        (Some("completed"), Some("cafe\u{301}\n"), Some(0)),
        "{r}"
    );
    let (r, _) = pane_run(&env, &pane, "echo after", &[]);
    assert_eq!(r["stdout"], "after\n", "{r}");
}

/// Plain bash draws no end-of-line mark: output shaped like zsh's, at the
/// pane's width, is the command's own.
#[test]
fn plain_bash_output_shaped_like_zshs_mark_is_kept() {
    let (env, _rc, pane) = integrated(&bash());
    let (r, _) = pane_run(&env, &pane, r"printf '%s%*s\r \r' REAL 79 ''", &[]);
    let want = format!("REAL{}\r \r", " ".repeat(79));
    assert_eq!(r["stdout"].as_str(), Some(want.as_str()), "{r}");
}

/// An empty prompt, and one ending in a newline, are prompts.
#[test]
fn a_prompt_that_ends_at_the_start_of_a_row_is_a_prompt() {
    let (env, _rc, pane) = integrated(&bash());
    for ps1 in ["PS1=''", "PS1='top\\n'"] {
        let floor = as_of(&env, &pane);
        type_line(&env, ps1);
        let r = result(
            &wait(&env, &pane, floor, "20s")
                .output()
                .expect("spawn shux"),
        );
        assert_eq!(r["outcome"], "completed", "{ps1}: {r}");
        let (r, code) = pane_run(&env, &pane, "echo next", &["--timeout", "10"]);
        assert_eq!(
            (r["stdout"].as_str(), code),
            (Some("next\n"), Some(0)),
            "{ps1}: {r}"
        );
    }
}

/// A shell started from the pane's own, integrated and at its prompt, is a
/// prompt: the line runs there.
#[test]
fn a_nested_integrated_shell_takes_the_line() {
    let env = Env::new(80, 24);
    let rc = tempfile::tempdir().expect("rc dir");
    let pane = start(&env, &rc, &bash(), true);
    let rcfile = rc.path().join("rc");
    let floor = as_of(&env, &pane);
    type_line(
        &env,
        &format!("{} --noprofile --rcfile '{}' -i", bash(), rcfile.display()),
    );
    // The child's first prompt ends the outer command.
    let r = result(
        &wait(&env, &pane, floor, "20s")
            .output()
            .expect("spawn shux"),
    );
    assert_eq!(r["ended_by"], "prompt", "{r}");
    let (r, code) = pane_run(
        &env,
        &pane,
        "echo child-$BASH_SUBSHELL$SHLVL",
        &["--timeout", "10"],
    );
    assert_eq!(code, Some(0), "{r}");
    assert!(
        r["stdout"]
            .as_str()
            .is_some_and(|s| s.starts_with("child-")),
        "{r}"
    );
}

/// A bash without a line editor reads its prompt in line mode; its prompt
/// mark says so, so the line is typed and runs.
#[test]
fn a_shell_without_a_line_editor_takes_the_line() {
    let env = Env::new(80, 24);
    let rc = tempfile::tempdir().expect("rc dir");
    let pane = start_with(&env, &rc, &bash(), true, "set +o emacs +o vi\n");
    let (r, code) = pane_run(&env, &pane, "echo plain", &["--timeout", "10"]);
    assert_eq!(
        (r["stdout"].as_str(), code),
        (Some("plain\n"), Some(0)),
        "{r}"
    );
}

/// `send-keys` from inside a pane never types into that same pane: keys, a
/// password among them, meant for another pane must not land in the caller.
#[test]
fn send_keys_into_the_callers_own_pane_is_refused() {
    let (env, _rc, pane) = integrated(&bash());
    let mut cmd = env.shux();
    cmd.env("SHUX_PANE_ID", &pane).args([
        "--format",
        "json",
        "pane",
        "send-keys",
        "-s",
        "ws",
        "--text",
        "own-pane-275",
    ]);
    let out = cmd.output().expect("spawn shux");
    assert!(
        !out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        said.contains("own_pane") || said.contains("the pane this command runs in"),
        "{said}"
    );
    assert!(
        !env.capture("ws", &pane).contains("own-pane-275"),
        "the keys reached the caller's pane"
    );
}

/// A timed-out result says, in the result itself, how to get the command's
/// end: the exact wait for an OSC 133 command.
#[test]
fn a_timed_out_result_says_how_to_wait_for_its_end() {
    let (env, _rc, pane) = integrated(&bash());
    let (r, _) = pane_run(&env, &pane, "sleep 3", &["--timeout", "1"]);
    assert_eq!(r["state"], "timed_out", "{r}");
    let as_of = r["as_of"]
        .as_u64()
        .unwrap_or_else(|| panic!("no as_of: {r}"));
    let hint = r["hint"].as_str().unwrap_or_else(|| panic!("no hint: {r}"));
    assert!(
        hint.contains(&format!(
            "pane wait-command {pane} --after {as_of} --output"
        )),
        "{hint}"
    );
}
