//! End to end: when shux ends a pane's program it ends the program's whole
//! process group (issue #255), against the shipped binary in an isolated
//! `XDG_RUNTIME_DIR`. Each pane's leader leaves behind a descendant in its
//! group that ignores SIGHUP; every way of ending the pane must take it too.

use std::path::PathBuf;
use std::process::{Command, Output};

use shux_testkit::running;

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
        assert!(
            out.status.success(),
            "{args:?} failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[track_caller]
    fn pane_id(&self, args: &[&str]) -> String {
        let mut full = vec!["--format", "json"];
        full.extend_from_slice(args);
        let v: serde_json::Value = serde_json::from_str(&self.ok(&full)).expect("json");
        v.pointer("/pane/id")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("no pane id in {v}"))
            .to_string()
    }

    fn file(&self, name: &str) -> PathBuf {
        self.root.path().join("work").join(name)
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self.run(&["daemon", "stop"]);
    }
}

/// A `sleep` argument no other run on this machine is using; it ends the
/// survivor's command line, which is how [`running`] knows it.
fn needle() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .subsec_nanos();
    format!(
        "8{:03}{:04}",
        std::process::id() % 1_000,
        (nanos / 1_000) % 10_000
    )
}

/// A HUP-immune descendant in the leader's process group: alive once
/// [`Survivor::spawned`] returns, killed on drop if teardown left it.
struct Survivor {
    pidfile: PathBuf,
    pong: PathBuf,
    needle: String,
    pid: Option<i32>,
}

impl Survivor {
    fn new(env: &Env, tag: &str) -> Self {
        Self {
            pidfile: env.file(&format!("{tag}.pid")),
            pong: env.file(&format!("{tag}.pong")),
            needle: needle(),
            pid: None,
        }
    }

    /// The pane program: leave the survivor behind, then keep the leader
    /// running until shux ends it.
    fn script(&self) -> String {
        format!(
            "sh -c 'trap \"\" HUP; echo $$ > {pidfile}; exec sleep {needle}' & sleep 900",
            pidfile = self.pidfile.display(),
            needle = self.needle
        )
    }

    /// The pane program: leave behind a survivor holding none of the
    /// terminal's descriptors, answering SIGUSR1 with a pong, then exit 0.
    fn detached_script(&self) -> String {
        format!(
            "sh -c 'trap \"\" HUP; trap \"echo pong > {pong}\" USR1; echo $$ > {pidfile}; \
             while :; do sleep 1 & wait; done' {needle} </dev/null >/dev/null 2>&1 & \
             until [ -s {pidfile} ]; do sleep 0.01; done",
            pong = self.pong.display(),
            pidfile = self.pidfile.display(),
            needle = self.needle
        )
    }

    /// The survivor still runs: signals arrive in order, so one shux already
    /// killed can never write the pong.
    fn answers(&self) -> bool {
        let pid = self.pid.expect("spawned() first");
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid),
            nix::sys::signal::Signal::SIGUSR1,
        );
        shux_testkit::poll_until(shux_testkit::BUDGET, || self.pong.exists())
    }

    #[track_caller]
    fn spawned(&mut self) {
        let pid = shux_testkit::wait_for("the survivor's pidfile", || {
            std::fs::read_to_string(&self.pidfile)
                .ok()?
                .trim()
                .parse::<i32>()
                .ok()
                .filter(|&pid| running(pid, &self.needle))
        });
        self.pid = Some(pid);
    }

    #[track_caller]
    fn assert_ended_by(&self, how: &str) {
        let pid = self.pid.expect("spawned() first");
        assert!(
            shux_testkit::poll_until(shux_testkit::BUDGET, || !running(pid, &self.needle)),
            "`{how}` left pid {pid} (`… {}`) running: its process group was not ended",
            self.needle
        );
    }
}

impl Drop for Survivor {
    fn drop(&mut self) {
        if let Some(pid) = self.pid.filter(|&pid| running(pid, &self.needle)) {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }
}

fn sh(script: &str) -> [&str; 3] {
    ["sh", "-c", script]
}

#[test]
fn pane_kill_ends_a_hup_immune_descendant() {
    let env = Env::new();
    let mut s = Survivor::new(&env, "pane");
    let script = s.script();
    let mut args = vec!["pane", "split", "-s", "w", "--"];
    args.extend(sh(&script));
    let pane = env.pane_id(&args);
    s.spawned();
    env.ok(&["pane", "kill", "-s", "w", "-p", &pane]);
    s.assert_ended_by("pane kill");
}

#[test]
fn window_kill_ends_a_hup_immune_descendant() {
    let env = Env::new();
    let mut s = Survivor::new(&env, "window");
    let script = s.script();
    let mut args = vec!["window", "create", "-s", "w", "-n", "doomed", "--"];
    args.extend(sh(&script));
    env.ok(&args);
    s.spawned();
    env.ok(&["window", "kill", "-s", "w", "--window", "doomed"]);
    s.assert_ended_by("window kill");
}

#[test]
fn session_kill_ends_a_hup_immune_descendant() {
    let env = Env::new();
    let mut s = Survivor::new(&env, "session");
    let script = s.script();
    let mut args = vec!["session", "create", "g", "-d", "--"];
    args.extend(sh(&script));
    env.ok(&args);
    s.spawned();
    env.ok(&["session", "kill", "g"]);
    s.assert_ended_by("session kill");
}

#[test]
fn daemon_shutdown_ends_a_hup_immune_descendant() {
    let env = Env::new();
    let mut s = Survivor::new(&env, "daemon");
    let script = s.script();
    let mut args = vec!["session", "create", "g", "-d", "--"];
    args.extend(sh(&script));
    env.ok(&args);
    s.spawned();
    env.ok(&["daemon", "stop"]);
    s.assert_ended_by("daemon stop");
}

/// The sweep waits for the group: a descendant that handles SIGHUP still gets
/// to finish its cleanup after the leader has already gone.
#[test]
fn a_descendant_that_handles_the_hangup_finishes_its_cleanup() {
    let env = Env::new();
    let marker = env.file("cleaned");
    let started = env.file("started");
    let script = format!(
        "sh -c 'trap \"sleep 0.1; echo cleaned > {marker}; exit 0\" HUP; : > {started}; sleep 900 & wait' & sleep 900",
        marker = marker.display(),
        started = started.display(),
    );
    let mut args = vec!["pane", "split", "-s", "w", "--"];
    args.extend(sh(&script));
    let pane = env.pane_id(&args);
    shux_testkit::wait_until("the descendant to install its trap", || started.exists());
    env.ok(&["pane", "kill", "-s", "w", "-p", &pane]);
    assert!(
        shux_testkit::poll_until(shux_testkit::BUDGET, || {
            std::fs::read_to_string(&marker).is_ok_and(|s| s.contains("cleaned"))
        }),
        "the descendant was killed before its SIGHUP cleanup finished"
    );
}

/// A program that exits by itself was not ended by shux, so what it left
/// behind outside the terminal is left alone.
#[test]
fn a_program_that_exits_by_itself_keeps_what_it_detached() {
    let env = Env::new();
    let mut s = Survivor::new(&env, "detached");
    let script = s.detached_script();
    let mut args = vec![
        "--format", "json", "pane", "split", "-s", "w", "--wait", "--",
    ];
    args.extend(sh(&script));
    let v: serde_json::Value = serde_json::from_str(&env.ok(&args)).expect("json");
    assert_eq!(v["result"]["exit_code"], 0, "{v}");
    s.spawned();
    assert!(
        s.answers(),
        "shux ended a descendant of a program it never ended"
    );
}

/// A program that outlives its terminal is ended by shux, and with it the
/// rest of its process group. Only `revoke(2)` closes a terminal under a
/// live program: shux holds a slave fd while the child lives.
#[cfg(target_os = "macos")]
#[test]
fn a_program_that_outlives_its_terminal_takes_its_group_with_it() {
    const PROGRAM: &str = r#"
import ctypes, os, signal, sys, time
pidfile, needle = sys.argv[1:]
signal.signal(signal.SIGHUP, signal.SIG_IGN)
if os.fork() == 0:
    null = os.open("/dev/null", os.O_RDWR)
    for fd in (0, 1, 2):
        os.dup2(null, fd)
    os.execvp("sh", ["sh", "-c", f"echo $$ > {pidfile}; exec sleep {needle}"])
while not os.path.exists(pidfile):
    time.sleep(0.01)
ctypes.CDLL(None).revoke(os.ttyname(0).encode())
signal.signal(signal.SIGHUP, signal.SIG_DFL)
null = os.open("/dev/null", os.O_RDWR)
for fd in (0, 1, 2):
    os.dup2(null, fd)
time.sleep(30)
"#;
    let env = Env::new();
    let mut s = Survivor::new(&env, "outlives");
    let pidfile = s.pidfile.display().to_string();
    let args = [
        "pane", "split", "-s", "w", "--", "python3", "-I", "-c", PROGRAM, &pidfile, &s.needle,
    ];
    env.ok(&args);
    s.spawned();
    s.assert_ended_by("a terminal closed under a live program");
}

/// A leader killed with output still queued on its terminal is reaped: its
/// exit must not wait on a reader teardown has already stopped.
#[test]
fn a_leader_killed_mid_output_is_reaped() {
    let env = Env::new();
    let pidfile = env.file("chatty.pid");
    let script = format!("echo $$ > {}; exec yes", pidfile.display());
    let mut args = vec!["pane", "split", "-s", "w", "--"];
    args.extend(sh(&script));
    let pane = env.pane_id(&args);
    let leader: i32 = shux_testkit::wait_for("the leader's pidfile", || {
        std::fs::read_to_string(&pidfile).ok()?.trim().parse().ok()
    });
    env.ok(&["pane", "kill", "-s", "w", "-p", &pane]);
    let gone = || {
        let out = Command::new("ps")
            .args(["-o", "stat=", "-p", &leader.to_string()])
            .output()
            .expect("ps");
        let listed = !out.stdout.iter().all(u8::is_ascii_whitespace);
        // `ps -p` of an absent pid exits 1 and prints nothing; anything else
        // is a scan that failed, not an answer.
        assert!(
            out.status.success()
                || (out.status.code() == Some(1) && !listed && out.stderr.is_empty()),
            "ps failed: {:?} {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        !listed
    };
    assert!(
        shux_testkit::poll_until(shux_testkit::BUDGET, gone),
        "the leader {leader} was never reaped"
    );
}

/// Group members forking new HUP-immune members right through the grace are
/// all ended: a fork that races one SIGKILL is caught by the next.
#[test]
fn members_forking_through_the_grace_are_all_ended() {
    let env = Env::new();
    let needle = needle();
    let started = env.file("storm.started");
    // In the script's path too, so a forked member not yet exec'd is counted.
    let storm = env.file(&format!("storm-{needle}.sh"));
    let log = env.file("storm.log");
    std::fs::write(
        &storm,
        format!(
            "go=0; trap 'go=1' HUP; : > {started}\n\
             while [ $go = 0 ]; do read -r -t 0.02 _ < /dev/zero; done\n\
             i=0; while [ $i -lt 1500 ]; do\n\
             (trap '' HUP; exec sleep {needle}) & i=$((i+1)); echo $i >> {log}\n\
             done\n",
            started = started.display(),
            log = log.display()
        ),
    )
    .expect("write storm");
    let script = format!("for n in 1 2 3; do bash {} & done; wait", storm.display());
    let mut args = vec!["pane", "split", "-s", "w", "--"];
    args.extend(sh(&script));
    let pane = env.pane_id(&args);
    shux_testkit::wait_until("the storm to arm", || started.exists());
    env.ok(&["pane", "kill", "-s", "w", "-p", &pane]);
    let left = || {
        let out = Command::new("ps")
            .args(["-axo", "pid=,command="])
            .output()
            .expect("ps");
        assert!(
            out.status.success(),
            "ps failed: an empty scan would read as none left"
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.contains(&needle))
            .filter_map(|l| l.split_whitespace().next()?.parse::<i32>().ok())
            .collect::<Vec<_>>()
    };
    let gone = shux_testkit::poll_until(shux_testkit::BUDGET, || left().is_empty());
    let forked = std::fs::read_to_string(&log).is_ok_and(|l| !l.is_empty());
    let survivors = left();
    for pid in &survivors {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(*pid),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    assert!(forked, "the storm never forked, so nothing was tested");
    assert!(gone, "forked members survived the teardown: {survivors:?}");
}

/// A scratch that `--max-runtime` ends was ended by shux: its exit says so,
/// though the lens reaper, not the pane's own teardown, sent the signal.
#[test]
fn a_scratch_ended_at_max_runtime_is_marked_as_ended_by_shux() {
    let env = Env::new();
    let out = env.run(&[
        "--format",
        "json",
        "lens",
        "run",
        "--max-runtime",
        "1s",
        "--wait",
        "--",
        "sleep",
        "30",
    ]);
    let run: serde_json::Value = serde_json::from_slice(&out.stdout).expect("lens run json");
    let pane = run["result"]["pane_id"]
        .as_str()
        .expect("pane id")
        .to_string();
    assert_eq!(run["result"]["signal"], 15, "{run}");
    let history = env.ok(&[
        "--format",
        "json",
        "events",
        "history",
        "-f",
        "pane.exited",
        "-n",
        "100",
    ]);
    let exited = history
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .map(|e| e["data"]["data"].clone())
        .find(|d| d["pane_id"] == pane.as_str() && d["signal"].is_number())
        .unwrap_or_else(|| panic!("no pane.exited with a signal for {pane}:\n{history}"));
    assert_eq!(
        exited["by_shux"], true,
        "shux's own --max-runtime kill read as the program's: {exited}"
    );
}

/// A scratch cleans up after itself: what its program leaves in its group,
/// detached from the terminal, is ended when the program exits by itself,
/// not left running until the TTL. (One still holding the terminal keeps the
/// pane open, so the TTL or `--max-runtime` ends it.)
#[test]
fn a_scratch_that_exits_takes_its_leftovers_with_it() {
    let env = Env::new();
    let mut s = Survivor::new(&env, "scratch");
    let script = format!(
        "sh -c 'trap \"\" HUP TERM; echo $$ > {pidfile}; exec sleep {needle}' \
         </dev/null >/dev/null 2>&1 & until [ -s {pidfile} ]; do sleep 0.01; done",
        pidfile = s.pidfile.display(),
        needle = s.needle
    );
    env.ok(&["lens", "run", "--ttl", "300s", "--", "sh", "-c", &script]);
    s.spawned();
    s.assert_ended_by("a scratch program exiting by itself");
}

/// An explicit `session kill` of a scratch keeps the lens's signal order:
/// SIGTERM first, so a program that cleans up on SIGTERM gets to.
#[test]
fn killing_a_scratch_session_sends_sigterm_first() {
    let env = Env::new();
    let marker = env.file("termed");
    let ready = env.file("ready");
    let script = format!(
        "trap 'echo termed > {m}; exit 0' TERM; : > {r}; while :; do sleep 0.05; done",
        m = marker.display(),
        r = ready.display()
    );
    let out = env.ok(&["--format", "json", "lens", "run", "--", "sh", "-c", &script]);
    let run: serde_json::Value = serde_json::from_str(&out).expect("lens run json");
    let session = run["result"]["session_id"]
        .as_str()
        .expect("session id")
        .to_string();
    shux_testkit::wait_until("the scratch to arm its trap", || ready.exists());
    env.ok(&["session", "kill", &session]);
    assert!(
        shux_testkit::poll_until(shux_testkit::BUDGET, || {
            std::fs::read_to_string(&marker).is_ok_and(|s| s.contains("termed"))
        }),
        "the scratch was ended without SIGTERM first"
    );
}
