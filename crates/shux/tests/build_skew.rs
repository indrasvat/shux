//! A CLI keeps working against a busy daemon of another build (#318).
//!
//! The "other build" is this binary's own daemon started with
//! `SHUX_TEST_DAEMON_GIT_SHA`, `_VERSION` and `_WIRE`, which debug builds
//! honour: it then answers the version check, and writes its build file, as
//! that build. Release builds compile the hook out, so these run in debug
//! builds only. `make test-skew` runs the same shapes between real builds.
#![cfg(debug_assertions)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use nix::sys::signal::kill;
use nix::unistd::Pid;

const DAEMON_IN_USE: i32 = 10;
const DAEMON_TOO_OLD: i32 = 11;
const OTHER_SHA: &str = "0ther00";

/// What the daemon a command starts says it is.
#[derive(Clone, Copy)]
struct Build {
    version: &'static str,
    wire: &'static str,
}

/// This release at another commit, of this wire generation.
const SIBLING: Build = Build {
    version: env!("CARGO_PKG_VERSION"),
    wire: "1",
};
/// An older release of this wire generation.
const OLDER: Build = Build {
    version: "0.0.1",
    wire: "1",
};
/// A newer release of this wire generation.
const NEWER: Build = Build {
    version: "999.0.0",
    wire: "1",
};
/// 0.64.0, before wire generations.
const V0_64: Build = Build {
    version: "0.64.0",
    wire: "-",
};
/// Another wire generation.
const NEXT_WIRE: Build = Build {
    version: "999.0.0",
    wire: "2",
};

struct Harness {
    bin: PathBuf,
    runtime: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        Self {
            bin: PathBuf::from(env!("CARGO_BIN_EXE_shux")),
            runtime: tempfile::tempdir().expect("temp runtime dir"),
        }
    }

    fn shux_at(&self, bin: &Path) -> Command {
        let mut cmd = Command::new(bin);
        cmd.env("XDG_RUNTIME_DIR", self.runtime.path())
            .env("NO_COLOR", "1")
            .env("SHELL", "/bin/sh")
            .env_remove("SHUX_SOCKET")
            .env_remove("SHUX")
            .env_remove("SHUX_HANDOFF")
            .env_remove("SHUX_TEST_DAEMON_GIT_SHA")
            .env_remove("SHUX_TEST_DAEMON_VERSION")
            .env_remove("SHUX_TEST_DAEMON_WIRE");
        cmd
    }

    fn shux(&self) -> Command {
        self.shux_at(&self.bin)
    }

    /// `args` as a hand-off runs them: handed to build `id` by a CLI of
    /// `caller` ("<version> <sha>"), in the very process it was set for
    /// (`exec` keeps the shell's pid).
    fn handed(&self, id: &str, caller: &str, args: &[&str]) -> Output {
        let shux = self.shux();
        let mut sh = Command::new("/bin/sh");
        for (k, v) in shux.get_envs() {
            match v {
                Some(v) => sh.env(k, v),
                None => sh.env_remove(k),
            };
        }
        sh.arg("-c")
            .arg(format!(r#"SHUX_HANDOFF="{id} $$ {caller}" exec "$0" "$@""#))
            .arg(shux.get_program())
            .args(args)
            .output()
            .expect("run")
    }

    fn run(&self, args: &[&str]) -> Output {
        self.shux().args(args).output().expect("run shux")
    }

    fn json(&self, args: &[&str]) -> (Option<i32>, serde_json::Value) {
        let out = self
            .shux()
            .arg("--format")
            .arg("json")
            .args(args)
            .output()
            .expect("run shux");
        let parsed = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "{args:?}: not JSON ({e}): {} {}",
                text(&out.stdout),
                text(&out.stderr)
            )
        });
        (out.status.code(), parsed)
    }

    /// Runs `args` from a client whose daemon, if it starts one, is `build`.
    fn run_as(&self, build: Build, args: &[&str]) -> Output {
        self.shux()
            .env("SHUX_TEST_DAEMON_GIT_SHA", OTHER_SHA)
            .env("SHUX_TEST_DAEMON_VERSION", build.version)
            .env("SHUX_TEST_DAEMON_WIRE", build.wire)
            .args(args)
            .output()
            .expect("run shux")
    }

    /// A daemon of `build` holding session `agent`, a shell; its pane.
    fn occupied_by(&self, build: Build) -> String {
        let out = self.run_as(
            build,
            &[
                "--format", "json", "session", "create", "agent", "-d", "--", "/bin/sh",
            ],
        );
        assert!(out.status.success(), "{}", text(&out.stderr));
        let created: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
        created["pane_id"].as_str().expect("pane_id").to_string()
    }

    fn pid_dir(&self) -> PathBuf {
        self.runtime.path().join("shux")
    }

    #[track_caller]
    fn daemon_pid(&self) -> i32 {
        shux_testkit::wait_for("a live daemon pidfile", || {
            live_pidfiles(&self.pid_dir()).into_iter().next()
        })
    }

    fn version(&self) -> serde_json::Value {
        self.json(&["version"]).1
    }

    /// The daemon's build file, once it has kept its build (just after it
    /// starts serving).
    #[track_caller]
    fn build_file(&self) -> serde_json::Value {
        let file = self.build_file_raw();
        assert!(file["build"]["exe"].is_string(), "no build kept: {file}");
        file
    }

    /// The daemon's build file once written, whether or not it kept a build.
    #[track_caller]
    fn build_file_raw(&self) -> serde_json::Value {
        let path = self.pid_dir().join("shux.build.json");
        shux_testkit::wait_for("the daemon's build file", || {
            serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()
        })
    }

    /// A private runtime dir as the daemon makes it, before any daemon runs.
    fn make_runtime_dir(&self) -> PathBuf {
        let dir = self.pid_dir();
        std::fs::create_dir_all(&dir).expect("runtime dir");
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .expect("chmod runtime dir");
        dir
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        for pid in live_pidfiles(&self.pid_dir()) {
            shux_testkit::terminate(pid);
        }
    }
}

fn live_pidfiles(dir: &Path) -> Vec<i32> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "pid"))
        .filter_map(|p| std::fs::read_to_string(p).ok()?.trim().parse().ok())
        .filter(|pid| alive(*pid))
        .collect()
}

fn alive(pid: i32) -> bool {
    kill(Pid::from_raw(pid), None).is_ok() && !shux_testkit::is_zombie(pid)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Truecolor, indexed and basic colour drawn in `pane` of `session` through
/// this CLI reach the other build's daemon and come back in its cells.
#[track_caller]
fn assert_colour_probes(h: &Harness, session: &str, pane: &str) {
    let probes = "printf '\\033[38;2;120;220;180mTRUE%sCOLOR\\033[0m \\033[38;5;208mINDE%sXED\\033[0m \\033[34mBA%sSIC\\033[0m\\n' '' '' ''\n";
    ok(
        &h.run(&[
            "pane",
            "send-keys",
            "-s",
            session,
            "-p",
            pane,
            "--text",
            probes,
        ]),
        "send the colour probes",
    );
    ok(
        &h.run(&[
            "pane",
            "wait-for",
            "-s",
            session,
            "-p",
            pane,
            "--text",
            "BASIC",
            "--timeout-ms",
            "20000",
        ]),
        "the probes to draw",
    );
    let (code, glanced) = h.json(&["pane", "glance", pane, "--cells"]);
    assert_eq!(code, Some(0), "{glanced}");
    let runs: Vec<&serde_json::Value> = glanced["result"]["cells"]["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|r| r["runs"].as_array().into_iter().flatten())
        .collect();
    for (word, fg) in [
        ("TRUECOLOR", serde_json::json!({ "rgb": [120, 220, 180] })),
        ("INDEXED", serde_json::json!({ "idx": 208 })),
        ("BASIC", serde_json::json!({ "idx": 4 })),
    ] {
        assert!(
            runs.iter().any(|r| r[1] == word && r[2]["fg"] == fg),
            "{word} did not come back in {fg}: {runs:?}"
        );
    }
}

#[track_caller]
fn ok(out: &Output, what: &str) -> String {
    assert!(
        out.status.success(),
        "{what}: exit {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        text(&out.stdout),
        text(&out.stderr)
    );
    text(&out.stdout)
}

/// The everyday shape: an agent's long-lived session on the old daemon, and
/// the updated CLI on PATH. Every common command runs against that daemon,
/// which is neither stopped nor replaced. Before #318 each one exited 10.
#[test]
fn every_common_command_runs_against_a_busy_daemon_of_another_build() {
    let h = Harness::new();
    let pane = h.occupied_by(SIBLING);
    let daemon = h.daemon_pid();
    assert_colour_probes(&h, "agent", &pane);

    let listed = ok(&h.run(&["session", "list"]), "session list");
    assert!(listed.contains("agent"), "{listed}");
    ok(&h.run(&["window", "list", "-s", "agent"]), "window list");
    ok(&h.run(&["pane", "list", "-s", "agent"]), "pane list");
    ok(
        &h.run(&[
            "pane",
            "send-keys",
            "-s",
            "agent",
            "-p",
            &pane,
            "--text",
            "echo sk\"\"ew-$((6*7))\n",
        ]),
        "send-keys",
    );
    ok(
        &h.run(&[
            "pane",
            "wait-for",
            "-s",
            "agent",
            "-p",
            &pane,
            "--text",
            "skew-42",
            "--timeout-ms",
            "20000",
        ]),
        "wait-for",
    );
    let captured = ok(
        &h.run(&["pane", "capture", "-s", "agent", "-p", &pane]),
        "capture",
    );
    assert!(captured.contains("skew-42"), "{captured}");
    let (code, ran) = h.json(&[
        "pane",
        "run",
        "-s",
        "agent",
        "-p",
        &pane,
        "--boundaries",
        "markers",
        "--timeout",
        "30s",
        "-c",
        "printf 'r%sn\\n' u",
    ]);
    assert_eq!(code, Some(0), "pane run: {ran}");
    assert!(
        ran["stdout"].as_str().unwrap_or("").contains("run"),
        "{ran}"
    );
    let (code, shot) = h.json(&["pane", "snapshot", "-s", "agent", "-p", &pane]);
    assert_eq!(code, Some(0), "snapshot");
    assert!(
        shot["png_base64"].as_str().is_some_and(|b| b.len() > 100),
        "no png"
    );
    ok(
        &h.run(&[
            "window", "create", "-s", "agent", "--name", "w2", "--", "/bin/sh",
        ]),
        "window create",
    );
    ok(
        &h.run(&["session", "create", "second", "-d", "--", "/bin/sh"]),
        "session create",
    );
    let (code, waited) = h.json(&[
        "session", "create", "job", "--wait", "--", "/bin/sh", "-c", "exit 7",
    ]);
    assert_eq!(code, Some(0), "session create --wait: {waited}");
    assert_eq!(waited["result"]["exit_code"], 7, "{waited}");
    ok(&h.run(&["session", "kill", "second"]), "session kill");

    let version = h.version();
    assert_eq!(version["daemon"]["compat"], "compatible", "{version}");
    assert_eq!(version["daemon"]["git_sha"], OTHER_SHA, "{version}");
    assert_eq!(h.daemon_pid(), daemon, "the daemon was replaced");
    assert!(alive(daemon));
}

/// A call the older daemon cannot serve (a method or key this build has and
/// it lacks) fails alone, as `daemon_too_old`, exit 11, naming what it lacks
/// and the way forward; the next call works. A name this build does not know
/// either is a mistake, reported as it is, against any build.
#[test]
fn a_call_the_daemon_cannot_serve_fails_alone_and_says_why() {
    let h = Harness::new();
    let pane = h.occupied_by(V0_64);

    let missing = h.run(&[
        "rpc",
        "call",
        shux_rpc::UPGRADE,
        "--params",
        r#"{"when":"idle"}"#,
    ]);
    assert_eq!(
        missing.status.code(),
        Some(DAEMON_TOO_OLD),
        "{}",
        text(&missing.stderr)
    );
    let said = text(&missing.stderr);
    for want in [
        "needs a newer shux daemon",
        OTHER_SHA,
        shux_rpc::UPGRADE,
        "daemon upgrade --when-idle",
    ] {
        assert!(said.contains(want), "lacks {want:?}: {said}");
    }
    ok(
        &h.run(&["pane", "capture", "-s", "agent", "-p", &pane]),
        "the next call",
    );

    for build in [V0_64, SIBLING] {
        let h = Harness::new();
        let pane = h.occupied_by(build);
        let typo = h.run(&["rpc", "call", "pane.teleport", "--params", "{}"]);
        assert_eq!(
            typo.status.code(),
            Some(2),
            "an unknown method: {}",
            text(&typo.stderr)
        );
        let params = serde_json::json!({ "pane_id": pane, "Pane_id": "x" }).to_string();
        let (code, refused) = h.json(&["rpc", "call", "pane.capture", "--params", &params]);
        assert_eq!(code, Some(2), "a misspelt key: {refused}");
        assert_eq!(
            refused["error"]["data"]["params"],
            serde_json::json!(["Pane_id"]),
            "{refused}"
        );
    }
}

/// An idle daemon of an older release is replaced; a newer release is never
/// replaced by this older CLI, idle or not, and is told a newer CLI exists
/// only by a newer one.
#[test]
fn only_an_older_idle_daemon_is_replaced() {
    let older = Harness::new();
    ok(
        &older.run_as(OLDER, &["session", "list"]),
        "start an older daemon",
    );
    let old = older.daemon_pid();
    ok(&older.run(&["session", "list"]), "this CLI");
    shux_testkit::wait_until("the older idle daemon to retire", || !alive(old));
    assert_eq!(older.version()["mismatch"], false);

    let newer = Harness::new();
    ok(
        &newer.run_as(NEWER, &["session", "list"]),
        "start a newer daemon",
    );
    let new = newer.daemon_pid();
    ok(&newer.run(&["session", "list"]), "this older CLI");
    ok(
        &newer.run(&["session", "create", "s", "-d", "--", "/bin/sh"]),
        "create on it",
    );
    assert_eq!(
        newer.daemon_pid(),
        new,
        "an older CLI replaced a newer daemon"
    );
    let v = newer.version();
    assert_eq!(v["daemon"]["superseded_by"], serde_json::Value::Null, "{v}");

    let busy = Harness::new();
    busy.occupied_by(OLDER);
    ok(&busy.run(&["session", "list"]), "a newer CLI");
    let v = busy.version();
    assert_eq!(
        v["daemon"]["superseded_by"],
        env!("CARGO_PKG_VERSION"),
        "{v}"
    );
}

/// A newer CLI that meets a busy daemon of an older release arms its
/// upgrade: the daemon retires as its last session ends, whoever ends it,
/// and the next command runs on the newer build. No older CLI that started
/// its own build again in between keeps it for long: the next newer CLI
/// arms it again.
#[test]
fn a_newer_cli_arms_the_upgrade_of_a_busy_older_daemon() {
    let h = Harness::new();
    h.occupied_by(OLDER);
    let old = h.daemon_pid();
    ok(&h.run(&["session", "list"]), "a newer CLI");
    let v = h.version();
    assert_eq!(v["daemon"]["upgrade_pending"], true, "{v}");
    assert!(alive(old), "a busy daemon was retired");
    ok(
        &h.run(&["session", "kill", "agent"]),
        "end the last session",
    );
    shux_testkit::wait_until("the older daemon to retire", || !alive(old));
    ok(
        &h.run(&["session", "create", "next", "-d", "--", "/bin/sh"]),
        "the next command",
    );
    assert_eq!(h.version()["daemon"]["compat"], "same");

    // Same release at another commit: nothing newer, nothing armed.
    let sibling = Harness::new();
    sibling.occupied_by(SIBLING);
    ok(&sibling.run(&["session", "list"]), "a sibling CLI");
    assert_eq!(sibling.version()["daemon"]["upgrade_pending"], false);

    // A newer CLI's retire, refused because a session landed after its
    // probe found the daemon idle, arms the upgrade all the same.
    let newer = r#"{"client":{"version":"999.0.0","git_sha":"x"}}"#;
    ok(
        &sibling.run(&["rpc", "call", "daemon.retire_if_idle", "--params", newer]),
        "a newer CLI's retire",
    );
    assert_eq!(sibling.version()["daemon"]["upgrade_pending"], true);
}

/// `daemon upgrade` in every state: same build, idle, busy, busy and
/// deferred, busy and forced, and a newer daemon.
#[test]
fn daemon_upgrade_in_every_state() {
    let same = Harness::new();
    ok(&same.run(&["session", "list"]), "start this build");
    let (code, r) = same.json(&["daemon", "upgrade"]);
    assert_eq!((code, &r["outcome"]), (Some(0), &"current".into()), "{r}");

    let absent = Harness::new();
    let (code, r) = absent.json(&["daemon", "upgrade", "--when-idle"]);
    assert_eq!((code, &r["outcome"]), (Some(0), &"absent".into()), "{r}");
    assert!(
        live_pidfiles(&absent.pid_dir()).is_empty(),
        "upgrade started a daemon"
    );

    let idle = Harness::new();
    ok(&idle.run_as(OLDER, &["session", "list"]), "start");
    let old = idle.daemon_pid();
    let (code, r) = idle.json(&["daemon", "upgrade"]);
    assert_eq!((code, &r["outcome"]), (Some(0), &"retired".into()), "{r}");
    shux_testkit::wait_until("the idle daemon to retire", || !alive(old));

    // Busy and of this release: nothing newer arms it, so it is refused.
    let busy = Harness::new();
    let pane = busy.occupied_by(SIBLING);
    let old = busy.daemon_pid();
    let (code, r) = busy.json(&["daemon", "upgrade"]);
    assert_eq!(
        (code, &r["outcome"], &r["sessions"]),
        (Some(1), &"busy".into(), &1.into()),
        "{r}"
    );
    assert!(alive(old), "a refused upgrade stopped the daemon");
    let older = Harness::new();
    older.occupied_by(OLDER);
    let (code, r) = older.json(&["daemon", "upgrade"]);
    assert_eq!(
        (code, &r["outcome"]),
        (Some(0), &"pending".into()),
        "a newer CLI's upgrade is armed: {r}"
    );

    let (code, r) = busy.json(&["daemon", "upgrade", "--when-idle"]);
    assert_eq!((code, &r["outcome"]), (Some(0), &"pending".into()), "{r}");
    let v = busy.version();
    assert_eq!(v["daemon"]["upgrade_pending"], true, "{v}");
    ok(
        &busy.run(&["pane", "capture", "-s", "agent", "-p", &pane]),
        "still serving",
    );
    ok(
        &busy.run(&["session", "kill", "agent"]),
        "end the last session",
    );
    shux_testkit::wait_until("the daemon to hand over", || !alive(old));
    ok(
        &busy.run(&["session", "create", "fresh", "-d", "--", "/bin/sh"]),
        "next command",
    );
    assert_eq!(
        busy.version()["mismatch"],
        false,
        "the next command started this build"
    );

    let forced = Harness::new();
    forced.occupied_by(OLDER);
    let old = forced.daemon_pid();
    let (code, r) = forced.json(&["daemon", "upgrade", "--force"]);
    assert_eq!((code, &r["outcome"]), (Some(0), &"stopped".into()), "{r}");
    // It no longer serves the socket; the process may take a moment to go.
    shux_testkit::wait_until("the forced daemon to exit", || !alive(old));

    let newer = Harness::new();
    newer.occupied_by(NEWER);
    let (code, r) = newer.json(&["daemon", "upgrade", "--when-idle"]);
    assert_eq!((code, &r["outcome"]), (Some(1), &"newer".into()), "{r}");

    let flags = absent.run(&["daemon", "upgrade", "--when-idle", "--force"]);
    assert_eq!(
        flags.status.code(),
        Some(2),
        "conflicting flags are a usage error"
    );
}

/// A pending upgrade retires the daemon in the step that ends its last
/// session: a create that reaches it afterwards is refused as `daemon_retiring`
/// (so a CLI sends it again to the successor), never half-applied.
#[test]
fn a_create_racing_a_pending_upgrade_lands_on_the_successor() {
    let h = Harness::new();
    h.occupied_by(OLDER);
    let old = h.daemon_pid();
    let socket = h.pid_dir().join("shux.sock");
    // Held open on the old daemon from before it retires.
    let mut conn = std::os::unix::net::UnixStream::connect(&socket).expect("connect");
    let (code, _) = h.json(&["daemon", "upgrade", "--when-idle"]);
    assert_eq!(code, Some(0));
    ok(
        &h.run(&["session", "kill", "agent"]),
        "end the last session",
    );

    let refused = call(
        &mut conn,
        "session.create",
        serde_json::json!({ "name": "late" }),
    );
    let kind = refused["error"]["data"]["kind"].clone();
    // The daemon may be gone already; a reply, if any, says nothing ran:
    // the graph refused it, or the daemon never dispatched it.
    if refused.get("gone").is_none() {
        assert!(
            kind == "daemon_retiring" || kind == "daemon_stopping",
            "{refused}"
        );
    }
    // Written as it retires, after the kill's reply: waited for, not raced.
    let path = h.pid_dir().join("shux.build.json");
    shux_testkit::wait_until("a retired daemon to leave its build file saying so", || {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .is_some_and(|f| f["pid"] == old && f["retiring"] == true)
    });
    shux_testkit::wait_until("the old daemon to exit", || !alive(old));
    ok(
        &h.run(&["session", "create", "late", "-d", "--", "/bin/sh"]),
        "create after",
    );
    let listed = ok(&h.run(&["session", "list"]), "list");
    assert!(
        listed.contains("late") && !listed.contains("agent"),
        "{listed}"
    );
}

/// Commands racing the moment a pending upgrade retires the daemon, from
/// other processes, all succeed: on the old daemon before it retires, or
/// once on its successor. Repeated, because the window is narrow.
#[test]
fn every_command_racing_the_retirement_succeeds() {
    for round in 0..20 {
        let h = Harness::new();
        h.occupied_by(OLDER);
        assert_eq!(h.json(&["daemon", "upgrade", "--when-idle"]).0, Some(0));
        let spawn = |args: &[&str]| {
            h.shux()
                .args(args)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("spawn")
        };
        let killer = spawn(&["session", "kill", "agent"]);
        let racers: Vec<_> = (0..4)
            .map(|i| match i % 2 {
                0 => spawn(&["session", "list"]),
                _ => spawn(&[
                    "session",
                    "create",
                    &format!("late{i}"),
                    "-d",
                    "--",
                    "/bin/sh",
                ]),
            })
            .collect();
        let killed = killer.wait_with_output().expect("kill");
        assert!(
            killed.status.success(),
            "round {round}: kill: {}",
            text(&killed.stderr)
        );
        for racer in racers {
            ok(
                &racer.wait_with_output().expect("racer"),
                &format!("round {round}: racing the retirement"),
            );
        }
        let after = ok(&h.run(&["session", "list"]), "list");
        assert!(
            after.contains("late1") && after.contains("late3"),
            "round {round}: {after}"
        );
    }
}

/// `daemon upgrade` against another wire generation is asked through the
/// daemon's own build, and does what it says: pending until the last
/// session ends, never "already this build".
#[test]
fn upgrading_another_wire_generation_really_upgrades() {
    let h = Harness::new();
    h.occupied_by(Build {
        version: "0.0.1",
        wire: "2",
    });
    h.build_file();
    let (code, r) = h.json(&["daemon", "upgrade", "--when-idle"]);
    assert_eq!((code, &r["outcome"]), (Some(0), &"pending".into()), "{r}");
    let old = h.daemon_pid();
    ok(
        &h.run(&["session", "kill", "agent"]),
        "end the last session",
    );
    shux_testkit::wait_until("the old generation to retire", || !alive(old));
    ok(&h.run(&["session", "list"]), "the next command");
    assert_eq!(h.version()["mismatch"], false);
}

/// `--force` ends a daemon even when it does not answer.
#[test]
fn force_ends_a_daemon_that_does_not_answer() {
    let h = Harness::new();
    h.occupied_by(OLDER);
    let pid = h.daemon_pid();
    kill(Pid::from_raw(pid), nix::sys::signal::Signal::SIGSTOP).expect("stop it");
    let out = h.run(&["daemon", "upgrade", "--force"]);
    let _ = kill(Pid::from_raw(pid), nix::sys::signal::Signal::SIGCONT);
    ok(&out, "force against a stopped daemon");
    shux_testkit::wait_until("the forced daemon to exit", || !alive(pid));
}

/// A call refused for skew exits 11 under `--format json` too; `rpc call`
/// keeps the raw envelope it got on stdout.
#[test]
fn a_refused_call_exits_11_under_json() {
    let h = Harness::new();
    h.occupied_by(V0_64);
    let (code, r) = h.json(&[
        "rpc",
        "call",
        shux_rpc::UPGRADE,
        "--params",
        r#"{"when":"idle"}"#,
    ]);
    assert_eq!(code, Some(DAEMON_TOO_OLD), "{r}");
    assert_eq!(r["error"]["code"], -32601, "{r}");
    assert_eq!(r["error"]["data"]["method"], shux_rpc::UPGRADE, "{r}");
}

/// 0.64.0 ignores keys it does not know: what it serves as asked works, a
/// template included; what it would not never reaches it.
#[test]
fn a_0_64_daemon_serves_only_what_it_serves_as_asked() {
    let h = Harness::new();
    let pane = h.occupied_by(V0_64);
    ok(&h.run(&["session", "list"]), "session list");
    assert_colour_probes(&h, "agent", &pane);
    ok(
        &h.run(&["pane", "capture", "-s", "agent", "-p", &pane]),
        "capture",
    );

    let spec = h.runtime.path().join("t.toml");
    std::fs::write(
        &spec,
        "[session]\nname = \"tpl\"\n[[windows]]\ntitle = \"w\"\n[[windows.panes]]\ncommand = [\"/bin/sh\"]\n",
    )
    .expect("template");
    ok(
        &h.run(&["state", "apply", spec.to_str().expect("utf-8")]),
        "a template 0.64.0 serves as written",
    );
    let listed = ok(&h.run(&["session", "list"]), "list");
    assert!(listed.contains("tpl"), "the template's session: {listed}");

    let (code, r) = h.json(&["daemon", "upgrade", "--when-idle"]);
    assert_eq!((code, &r["outcome"]), (Some(0), &"automatic".into()), "{r}");
    assert_eq!(h.version()["daemon"]["compat"], "legacy_0_64");
}

/// The daemon keeps a file of its own build: deleting the installed binary
/// leaves it serving, and its build file names the kept file, which outlives
/// the install.
#[test]
fn deleting_the_installed_binary_leaves_the_daemon_serving() {
    let h = Harness::new();
    let install = tempfile::tempdir().expect("install dir");
    let installed = install.path().join("shux");
    std::fs::copy(&h.bin, &installed).expect("install a copy");
    let started = h
        .shux_at(&installed)
        .args(["session", "create", "agent", "-d", "--", "/bin/sh"])
        .output()
        .expect("run");
    ok(&started, "start from the installed copy");
    let daemon = h.daemon_pid();
    let builds = h.pid_dir().join("builds");
    let file = h.build_file();
    assert_eq!(file["pid"], daemon);
    let kept = PathBuf::from(file["build"]["exe"].as_str().expect("exe"));
    assert!(kept.starts_with(&builds), "{file}");

    std::fs::remove_file(&installed).expect("uninstall");
    ok(&h.run(&["session", "list"]), "after the install is gone");
    assert!(kept.exists(), "the kept copy went with the install");
    let kept_version = ok(
        &Command::new(&kept)
            .args(["--format", "json", "version"])
            .env("XDG_RUNTIME_DIR", h.runtime.path())
            .output()
            .expect("run kept"),
        "the kept build runs",
    );
    assert!(
        kept_version.contains(env!("CARGO_PKG_VERSION")),
        "{kept_version}"
    );
    assert_eq!(h.daemon_pid(), daemon);

    ok(&h.run(&["daemon", "stop"]), "stop");
    shux_testkit::wait_until("the daemon to exit", || !alive(daemon));
    assert!(
        !h.pid_dir().join("shux.build.json").exists(),
        "the build file outlived the daemon"
    );
}

/// Another wire generation: the command is handed to the daemon's own kept
/// build, once; a handed-off command never hands off again, and never starts
/// a daemon of its own.
#[test]
fn another_wire_generation_runs_the_command_with_the_daemons_own_build() {
    let h = Harness::new();
    h.occupied_by(NEXT_WIRE);
    let daemon = h.daemon_pid();
    h.build_file();

    let listed = ok(&h.run(&["session", "list"]), "handed off");
    assert!(listed.contains("agent"), "{listed}");
    assert_eq!(h.daemon_pid(), daemon);

    let kept = format!("999.0.0-{OTHER_SHA}");
    let caller = "1.0.0 c4113r";
    // Told it was handed to a build the daemon is not: refused, not a loop.
    let wrong = h.handed("0.0.0-nobody", caller, &["session", "list"]);
    assert_eq!(
        wrong.status.code(),
        Some(DAEMON_IN_USE),
        "{}",
        text(&wrong.stderr)
    );

    // A command the daemon's own build does not know, as one written for a
    // newer build would be: said as such, not as a usage error.
    let unknown = h.handed(&kept, caller, &["pane", "teleport"]);
    assert_eq!(
        unknown.status.code(),
        Some(DAEMON_TOO_OLD),
        "{}",
        text(&unknown.stderr)
    );
    assert!(
        text(&unknown.stderr).contains("needs a newer shux"),
        "{}",
        text(&unknown.stderr)
    );

    // Set by hand, or inherited by another process: not a hand-off.
    let typo = h
        .shux()
        .env("SHUX_HANDOFF", format!("{kept} 1 {caller}"))
        .args(["pane", "teleport"])
        .output()
        .expect("run");
    assert_eq!(typo.status.code(), Some(2), "{}", text(&typo.stderr));
    let fresh = Harness::new();
    ok(
        &fresh
            .shux()
            .env("SHUX_HANDOFF", format!("{kept} 1 {caller}"))
            .args(["session", "list"])
            .output()
            .expect("run"),
        "an exported SHUX_HANDOFF starts a daemon as any command does",
    );

    let empty = Harness::new();
    let lost = empty.handed(&kept, caller, &["session", "list"]);
    assert!(!lost.status.success());
    assert!(
        text(&lost.stderr).contains("handed to"),
        "{}",
        text(&lost.stderr)
    );
    assert!(
        live_pidfiles(&empty.pid_dir()).is_empty(),
        "a handed-off command started a daemon"
    );
}

/// A handed-off command speaks for the CLI that handed it, not the daemon's
/// own build that runs it: a newer caller may switch the daemon, and the
/// daemon learns a newer shux exists.
#[test]
fn a_handed_off_command_speaks_for_the_cli_that_handed_it() {
    let h = Harness::new();
    h.occupied_by(NEXT_WIRE);
    let kept = format!("999.0.0-{OTHER_SHA}");
    let out = h.handed(
        &kept,
        "1000.0.0 c4113r",
        &["--format", "json", "daemon", "upgrade", "--when-idle"],
    );
    let r: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("not JSON ({e}): {}", text(&out.stderr)));
    assert_eq!(
        (out.status.code(), &r["outcome"]),
        (Some(0), &"pending".into()),
        "a newer caller was refused as older: {r}"
    );
    assert_eq!(r["client_version"], "1000.0.0", "{r}");
    assert_eq!(h.version()["daemon"]["superseded_by"], "1000.0.0");
}

/// A kept build that is not the file the daemon recorded is never run: the
/// command is refused as before rather than handed to whatever is there.
#[test]
fn a_kept_build_that_is_not_the_recorded_file_is_never_run() {
    let h = Harness::new();
    h.occupied_by(NEXT_WIRE);
    let file = h.build_file();
    let kept = PathBuf::from(file["build"]["exe"].as_str().expect("exe"));
    let marker = h.runtime.path().join("ran");
    std::fs::remove_file(&kept).expect("remove the kept copy");
    std::fs::write(&kept, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).expect("swap");
    std::fs::set_permissions(&kept, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("chmod");

    let listed = h.run(&["session", "list"]);
    assert_eq!(
        listed.status.code(),
        Some(DAEMON_IN_USE),
        "{}",
        text(&listed.stderr)
    );
    assert!(!marker.exists(), "ran a file the daemon never recorded");
}

/// A `builds` that is a symlink is never followed: nothing behind it is
/// swept, and no build is kept through it.
#[test]
fn a_symlinked_builds_dir_is_refused_not_swept() {
    let h = Harness::new();
    let victim = tempfile::tempdir().expect("victim");
    let precious = victim.path().join("precious");
    std::fs::create_dir(&precious).expect("dir");
    std::fs::write(precious.join("notes"), b"keep").expect("file");
    std::fs::File::open(&precious)
        .expect("open")
        .set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH))
        .expect("age it");
    std::os::unix::fs::symlink(victim.path(), h.make_runtime_dir().join("builds")).expect("plant");
    ok(
        &h.run(&["session", "create", "a", "-d", "--", "/bin/sh"]),
        "start",
    );
    let file = h.build_file_raw();
    assert!(precious.join("notes").exists(), "swept through the symlink");
    assert!(
        file["build"]["exe"].is_null(),
        "kept a build through it: {file}"
    );
    assert!(file["build"]["error"].is_string(), "{file}");
}

/// A file planted where the daemon keeps its build, of the right length, is
/// never taken for it: the daemon keeps its own bytes, and a hand-off never
/// runs the plant.
#[test]
fn a_planted_build_is_never_taken_for_the_daemons() {
    let h = Harness::new();
    let install = tempfile::tempdir().expect("install dir");
    let installed = install.path().join("shux");
    std::fs::copy(&h.bin, &installed).expect("install");
    let start = |build: Option<Build>| {
        let mut cmd = h.shux_at(&installed);
        if let Some(b) = build {
            cmd.env("SHUX_TEST_DAEMON_GIT_SHA", OTHER_SHA)
                .env("SHUX_TEST_DAEMON_VERSION", b.version)
                .env("SHUX_TEST_DAEMON_WIRE", b.wire);
        }
        ok(
            &cmd.args(["session", "create", "agent", "-d", "--", "/bin/sh"])
                .output()
                .expect("run"),
            "start",
        );
    };
    start(None);
    let kept = PathBuf::from(h.build_file()["build"]["exe"].as_str().expect("exe"));
    // A fresh daemon next, not this one's workspace restored.
    ok(&h.run(&["daemon", "stop", "--discard"]), "stop");

    let marker = h.runtime.path().join("ran");
    std::fs::remove_file(&kept).expect("remove");
    std::fs::write(&kept, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).expect("plant");
    let len = std::fs::metadata(&installed).expect("len").len();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&kept)
        .expect("open")
        .set_len(len)
        .expect("pad");
    std::fs::set_permissions(&kept, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("chmod");

    let _ = std::fs::remove_file(h.pid_dir().join("shux.build.json"));
    start(Some(NEXT_WIRE));
    let file = h.build_file();
    let _ = h.run(&["session", "list"]);
    assert!(!marker.exists(), "ran the planted file");
    let now_kept = PathBuf::from(file["build"]["exe"].as_str().expect("exe"));
    assert_eq!(
        std::fs::read(&now_kept).expect("kept"),
        std::fs::read(&installed).expect("installed"),
        "the kept build is not the daemon's bytes"
    );
}

/// An install writable by its group (a shared `0775` prefix) is still kept:
/// as a private file no one else can write.
#[test]
fn a_group_writable_install_is_still_kept() {
    let h = Harness::new();
    let install = tempfile::tempdir().expect("install dir");
    let installed = install.path().join("shux");
    std::fs::copy(&h.bin, &installed).expect("install");
    std::fs::set_permissions(
        &installed,
        std::os::unix::fs::PermissionsExt::from_mode(0o775),
    )
    .expect("chmod 775");
    let started = h
        .shux_at(&installed)
        .args(["session", "create", "a", "-d", "--", "/bin/sh"])
        .output()
        .expect("run");
    ok(&started, "start");
    let file = h.build_file_raw();
    let exe = file["build"]["exe"]
        .as_str()
        .unwrap_or_else(|| panic!("nothing kept: {file}"));
    let mode = std::os::unix::fs::PermissionsExt::mode(
        &std::fs::metadata(exe).expect("kept").permissions(),
    );
    assert_eq!(
        mode & 0o022,
        0,
        "the kept build is writable by others: {mode:o}"
    );
    let installed_mode = std::os::unix::fs::PermissionsExt::mode(
        &std::fs::metadata(&installed)
            .expect("installed")
            .permissions(),
    );
    assert_eq!(
        installed_mode & 0o777,
        0o775,
        "keeping it changed the install"
    );
}

/// `shux version` says where the daemon's build is kept, or why it is not.
#[test]
fn version_says_why_no_build_was_kept() {
    let h = Harness::new();
    std::fs::write(h.make_runtime_dir().join("builds"), b"not a dir").expect("plant");
    ok(
        &h.run(&["session", "create", "a", "-d", "--", "/bin/sh"]),
        "start",
    );
    h.build_file_raw();
    let v = h.version();
    assert!(v["daemon"]["build"]["error"].is_string(), "{v}");
}

/// One request on `conn`; `{"gone": ...}` when it is closed.
fn call(
    conn: &mut std::os::unix::net::UnixStream,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    use std::io::{Read, Write};
    let req = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let req = serde_json::to_vec(&req).expect("json");
    let mut len = [0u8; 4];
    let sent = conn
        .write_all(&(req.len() as u32).to_be_bytes())
        .and_then(|()| conn.write_all(&req))
        .and_then(|()| conn.read_exact(&mut len));
    if let Err(e) = sent {
        return serde_json::json!({ "gone": e.to_string() });
    }
    let mut resp = vec![0u8; u32::from_be_bytes(len) as usize];
    if let Err(e) = conn.read_exact(&mut resp) {
        return serde_json::json!({ "gone": e.to_string() });
    }
    serde_json::from_slice(&resp).expect("json response")
}
