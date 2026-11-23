//! A CLI of another build never stops a daemon that holds sessions (#277).
//!
//! The "other build" is this binary's own daemon started with
//! `SHUX_TEST_DAEMON_GIT_SHA`, which debug builds honour: it then answers the
//! version check as another commit, or with no version at all for `-`.
//! Release builds compile that hook out, so these run in debug builds only.
#![cfg(debug_assertions)]

use std::fs::File;
use std::io::Read;
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

/// Exit code of a command refused because the daemon is another build and may
/// hold sessions.
const DAEMON_IN_USE: i32 = 10;

const OTHER_SHA: &str = "0ther00";

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

    fn shux(&self) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.env("XDG_RUNTIME_DIR", self.runtime.path())
            .env("NO_COLOR", "1")
            .env("SHELL", "/bin/sh")
            .env_remove("SHUX_SOCKET")
            .env_remove("SHUX")
            .env_remove("SHUX_TEST_DAEMON_GIT_SHA");
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.shux().args(args).output().expect("run shux")
    }

    /// Runs `args` from a client whose daemon, if it starts one, answers the
    /// version check with `sha`.
    fn run_as_other_build(&self, sha: &str, args: &[&str]) -> Output {
        self.shux()
            .env("SHUX_TEST_DAEMON_GIT_SHA", sha)
            .args(args)
            .output()
            .expect("run shux")
    }

    fn pid_dir(&self) -> PathBuf {
        self.runtime.path().join("shux")
    }

    /// The pid of the one live daemon this runtime dir holds.
    #[track_caller]
    fn daemon_pid(&self) -> i32 {
        shux_testkit::wait_for("a live daemon pidfile", || {
            live_pidfiles(&self.pid_dir()).into_iter().next()
        })
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

/// A session on a daemon of another build, running `sleep <unique>`; the
/// sleep's pid and its needle.
fn occupy(h: &Harness) -> (i32, String) {
    let pidfile = h.runtime.path().join("pane.pid");
    let secs = format!("{}", 864_000 + std::process::id());
    let script = format!("echo $$ > '{}'; exec sleep {secs}", pidfile.display());
    let created = h.run_as_other_build(
        OTHER_SHA,
        &[
            "session", "create", "victim", "-d", "--", "sh", "-c", &script,
        ],
    );
    assert!(created.status.success(), "{}", text(&created.stderr));
    let pid = shux_testkit::wait_for("the pane's pidfile", || {
        std::fs::read_to_string(&pidfile).ok()?.trim().parse().ok()
    });
    let needle = format!("sleep {secs}");
    shux_testkit::wait_until("the pane's sleep to exec", || {
        shux_testkit::running(pid, &needle)
    });
    (pid, needle)
}

#[test]
fn a_daemon_of_another_build_holding_a_session_is_refused_not_stopped() {
    let h = Harness::new();
    let (pane, needle) = occupy(&h);
    let daemon = h.daemon_pid();

    let listed = h.run(&["session", "list"]);
    let stderr = text(&listed.stderr);
    assert_eq!(
        listed.status.code(),
        Some(DAEMON_IN_USE),
        "session list against an occupied daemon of another build: stdout={} stderr={stderr}",
        text(&listed.stdout)
    );
    for want in [
        OTHER_SHA,
        shux_rpc::GIT_SHA,
        "1 session",
        " daemon stop",
        "the next shux command starts this build",
    ] {
        assert!(stderr.contains(want), "refusal lacks {want:?}: {stderr}");
    }
    assert!(alive(daemon), "the daemon was stopped: {stderr}");
    assert!(
        shux_testkit::running(pane, &needle),
        "the pane's process was killed"
    );

    let json = h.run(&["--format", "json", "session", "list"]);
    assert_eq!(json.status.code(), Some(DAEMON_IN_USE));
    let parsed: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap_or_else(|e| {
        panic!(
            "not JSON ({e}): {} {}",
            text(&json.stdout),
            text(&json.stderr)
        )
    });
    let data = &parsed["error"]["data"];
    assert_eq!(parsed["error"]["code"], -32015, "{parsed}");
    assert_eq!(data["kind"], "daemon_in_use", "{parsed}");
    assert_eq!(data["exit_code"], DAEMON_IN_USE, "{parsed}");
    assert_eq!(data["daemon_git_sha"], OTHER_SHA, "{parsed}");
    assert_eq!(
        data["daemon_version"],
        env!("CARGO_PKG_VERSION"),
        "{parsed}"
    );
    assert_eq!(data["client_git_sha"], shux_rpc::GIT_SHA, "{parsed}");
    assert_eq!(
        data["client_version"],
        env!("CARGO_PKG_VERSION"),
        "{parsed}"
    );
    assert_eq!(data["sessions"], 1, "{parsed}");

    let version = text(&h.run(&["version"]).stdout);
    assert!(
        version.contains("holds 1 session") && version.contains(" daemon stop"),
        "version says what the next command does: {version}"
    );
    let plain = text(&h.run(&["--format", "plain", "version"]).stdout);
    assert!(
        plain.contains("\tmismatch\t1\n"),
        "plain version counts the sessions too: {plain:?}"
    );

    let attach = attach_on_a_pty(&h, "victim");
    assert_eq!(
        attach.0,
        Some(DAEMON_IN_USE),
        "session attach: {}",
        attach.1
    );

    assert!(alive(daemon), "the daemon was stopped");
    assert!(
        shux_testkit::running(pane, &needle),
        "the pane's process was killed"
    );

    // The explicit way forward still works against the other build.
    let stopped = h.run(&["daemon", "stop"]);
    assert!(stopped.status.success(), "{}", text(&stopped.stderr));
    shux_testkit::wait_until("the old daemon to exit", || !alive(daemon));
    let after = h.run(&["session", "list"]);
    assert!(after.status.success(), "{}", text(&after.stderr));
}

/// A scratch session is hidden from the default listing, and is still work.
#[test]
fn a_daemon_of_another_build_holding_only_a_scratch_session_is_refused() {
    let h = Harness::new();
    let secs = format!("{}", 865_000 + std::process::id());
    let ran = h.run_as_other_build(OTHER_SHA, &["lens", "run", "--", "sleep", &secs]);
    assert!(ran.status.success(), "{}", text(&ran.stderr));
    let daemon = h.daemon_pid();

    let listed = h.run(&["--format", "json", "session", "list"]);
    assert_eq!(
        listed.status.code(),
        Some(DAEMON_IN_USE),
        "{}",
        text(&listed.stdout)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("json");
    assert_eq!(parsed["error"]["data"]["sessions"], 1, "{parsed}");
    assert!(alive(daemon), "the daemon was stopped");
}

#[test]
fn an_empty_daemon_of_another_build_is_replaced() {
    let h = Harness::new();
    let started = h.run_as_other_build(OTHER_SHA, &["session", "list"]);
    assert!(started.status.success(), "{}", text(&started.stderr));
    let old = h.daemon_pid();
    let note = text(&h.run(&["version"]).stdout);
    assert!(
        note.contains("holds no sessions now, so the next shux command replaces it"),
        "{note}"
    );

    let listed = h.run(&["session", "list"]);
    assert!(
        listed.status.success(),
        "an empty daemon of another build was not replaced: {}",
        text(&listed.stderr)
    );
    shux_testkit::wait_until("the old daemon to exit", || !alive(old));
    let version = h.run(&["--format", "json", "version"]);
    let parsed: serde_json::Value = serde_json::from_slice(&version.stdout).expect("json");
    assert_eq!(parsed["mismatch"], false, "{parsed}");
}

#[test]
fn a_daemon_whose_version_answer_is_unreadable_is_left_alone() {
    let h = Harness::new();
    let started = h.run_as_other_build("-", &["session", "list"]);
    assert!(started.status.success(), "{}", text(&started.stderr));
    let daemon = h.daemon_pid();

    let listed = h.run(&["session", "list"]);
    let stderr = text(&listed.stderr);
    assert_eq!(listed.status.code(), Some(DAEMON_IN_USE), "{stderr}");
    assert!(
        stderr.contains(" daemon stop"),
        "the refusal names the way forward: {stderr}"
    );
    assert!(alive(daemon), "a daemon it could not identify was stopped");
}

/// The stop the refusal names reaches the daemon it refused, not the default
/// one beside it.
#[test]
fn the_refusal_names_a_stop_for_the_socket_it_refused() {
    let h = Harness::new();
    let ours = h.run(&["session", "list"]);
    assert!(ours.status.success(), "{}", text(&ours.stderr));
    let default_daemon = h.daemon_pid();

    let custom = h.runtime.path().join("its here.sock");
    let custom = custom.to_str().expect("utf-8 path");
    let secs = format!("{}", 866_000 + std::process::id());
    let created = h.run_as_other_build(
        OTHER_SHA,
        &[
            "--socket", custom, "session", "create", "victim", "-d", "--", "sleep", &secs,
        ],
    );
    assert!(created.status.success(), "{}", text(&created.stderr));
    let other = shux_testkit::wait_for("the other build's daemon", || {
        live_pidfiles(&h.pid_dir())
            .into_iter()
            .find(|p| *p != default_daemon)
    });

    let listed = h.run(&["--socket", custom, "session", "list"]);
    let stderr = text(&listed.stderr);
    assert_eq!(listed.status.code(), Some(DAEMON_IN_USE), "{stderr}");
    let stop = stderr
        .split("Run `")
        .nth(1)
        .and_then(|rest| rest.split('`').next())
        .unwrap_or_else(|| panic!("the refusal names no command to run: {stderr}"));
    let version = text(&h.run(&["--socket", custom, "version"]).stdout);
    assert!(
        version.contains(stop),
        "version names {stop:?} too: {version}"
    );

    // As a user would run it: pasted into a shell with `shux` on PATH and
    // none of this test's environment.
    let bin_dir = tempfile::tempdir().expect("bin dir");
    std::os::unix::fs::symlink(&h.bin, bin_dir.path().join("shux")).expect("symlink shux");
    let ran = Command::new("/bin/sh")
        .args(["-c", stop])
        .env_clear()
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", bin_dir.path().display()),
        )
        .env("HOME", h.runtime.path())
        .output()
        .expect("run the named stop");
    assert!(
        ran.status.success(),
        "`{stop}`: {} {}",
        text(&ran.stdout),
        text(&ran.stderr)
    );
    shux_testkit::wait_until("the refused daemon to exit", || !alive(other));
    assert!(
        alive(default_daemon),
        "`{stop}` stopped the default daemon instead"
    );
}

/// `lens gate` refuses like every other command, before it writes anything.
#[test]
fn lens_gate_is_refused_before_it_writes_anything() {
    let h = Harness::new();
    let (pane, needle) = occupy(&h);
    let work = tempfile::tempdir().expect("work dir");
    let scenario = work.path().join("s.toml");
    std::fs::write(&scenario, "name = \"s\"\ncommand = [\"true\"]\n").expect("scenario");
    let trace = work.path().join("trace.ndjson");
    std::fs::write(&trace, "kept\n").expect("trace");
    let path = |p: &Path| p.to_str().expect("utf-8 path").to_string();

    let gate = h.run(&[
        "--format",
        "json",
        "lens",
        "gate",
        &path(&scenario),
        "--trace",
        &path(&trace),
    ]);
    assert_eq!(
        gate.status.code(),
        Some(DAEMON_IN_USE),
        "{} {}",
        text(&gate.stdout),
        text(&gate.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&gate.stdout)
        .unwrap_or_else(|e| panic!("not JSON ({e}): {}", text(&gate.stdout)));
    assert_eq!(parsed["error"]["code"], -32015, "{parsed}");
    assert_eq!(
        std::fs::read_to_string(&trace).expect("trace"),
        "kept\n",
        "the refused gate rewrote its trace"
    );

    let init = h.run(&["lens", "gate", "init", "demo", "--dir", &path(work.path())]);
    let out = text(&init.stdout);
    assert_eq!(
        init.status.code(),
        Some(DAEMON_IN_USE),
        "{out} {}",
        text(&init.stderr)
    );
    assert!(!out.contains("scaffolded"), "{out}");
    assert!(
        !work.path().join("demo.toml").exists(),
        "the refused init left a scenario behind"
    );
    assert!(
        shux_testkit::running(pane, &needle),
        "the pane's process was killed"
    );
}

/// A daemon that does not answer the gate's check leaves no scaffold behind.
#[test]
fn gate_init_writes_nothing_when_the_daemon_does_not_answer() {
    let h = Harness::new();
    let started = h.run(&["session", "list"]);
    assert!(started.status.success(), "{}", text(&started.stderr));
    let daemon = h.daemon_pid();
    kill(Pid::from_raw(daemon), Signal::SIGSTOP).expect("stop the daemon");
    let work = tempfile::tempdir().expect("work dir");
    let init = h
        .shux()
        .env("SHUX_PEER_PATIENCE_MS", "150")
        .args(["lens", "gate", "init", "timedout", "--dir"])
        .arg(work.path())
        .output()
        .expect("run gate init");
    kill(Pid::from_raw(daemon), Signal::SIGCONT).expect("resume the daemon");
    let out = text(&init.stdout);
    assert!(!init.status.success(), "{out}");
    assert!(!out.contains("scaffolded"), "{out}");
    assert!(
        !work.path().join("timedout.toml").exists(),
        "gate init wrote its scenario before the daemon answered"
    );
}

/// The gate's check retires nothing: an idle daemon of another build is still
/// there when the gate stops at its own validation.
#[test]
fn the_gate_check_retires_nothing_before_the_gate_validates() {
    let h = Harness::new();
    let started = h.run_as_other_build(OTHER_SHA, &["session", "list"]);
    assert!(started.status.success(), "{}", text(&started.stderr));
    let old = h.daemon_pid();
    let work = tempfile::tempdir().expect("work dir");
    let missing = work.path().join("missing.toml");
    let gate = h.run(&["lens", "gate", missing.to_str().expect("utf-8 path")]);
    assert_eq!(gate.status.code(), Some(2), "{}", text(&gate.stderr));
    // Straight to the old daemon, not through a CLI that would replace it: a
    // retired one refuses this, or is gone.
    let created = raw_call(
        &h.pid_dir().join("shux.sock"),
        "session.create",
        serde_json::json!({ "name": "after" }),
    );
    assert!(
        created.get("result").is_some(),
        "a gate that never ran retired the daemon: {created}"
    );
    assert!(alive(old));
}

/// One JSON-RPC request on a fresh connection to `socket`; the response, or
/// `{"gone": ...}` when there is none.
fn raw_call(socket: &Path, method: &str, params: serde_json::Value) -> serde_json::Value {
    use std::io::Write;
    let gone = |e: std::io::Error| serde_json::json!({ "gone": e.to_string() });
    let mut conn = match std::os::unix::net::UnixStream::connect(socket) {
        Ok(conn) => conn,
        Err(e) => return gone(e),
    };
    let req = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let req = serde_json::to_vec(&req).expect("json");
    let mut len = [0u8; 4];
    let read = conn
        .write_all(&(req.len() as u32).to_be_bytes())
        .and_then(|()| conn.write_all(&req))
        .and_then(|()| conn.read_exact(&mut len));
    if let Err(e) = read {
        return gone(e);
    }
    let mut resp = vec![0u8; u32::from_be_bytes(len) as usize];
    if let Err(e) = conn.read_exact(&mut resp) {
        return gone(e);
    }
    serde_json::from_slice(&resp).expect("json response")
}

/// `shux session attach <session>` on a PTY that is its controlling
/// terminal; its exit code and what it wrote.
fn attach_on_a_pty(h: &Harness, session: &str) -> (Option<i32>, String) {
    let pty = nix::pty::openpty(None, None).expect("openpty");
    let dup = |fd: &OwnedFd| Stdio::from(fd.try_clone().expect("dup slave"));
    let mut cmd = h.shux();
    cmd.env("TERM", "xterm-256color")
        .args(["session", "attach", session])
        .stdin(dup(&pty.slave))
        .stdout(dup(&pty.slave))
        .stderr(dup(&pty.slave));
    // SAFETY: only async-signal-safe calls between fork and exec. The PTY
    // becomes the child's controlling terminal, so an attach that gets as far
    // as raw mode takes it and not the runner's.
    unsafe {
        cmd.pre_exec(|| {
            nix::unistd::setsid()?;
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().expect("spawn attach");
    // The Command holds dups of the slave too: the master reads EIO only once
    // every slave fd outside the child is closed.
    drop(cmd);
    drop(pty.slave);
    let mut master = File::from(pty.master);
    let reader = std::thread::spawn(move || {
        let mut seen = Vec::new();
        // EIO once the child, the session leader, has exited.
        let _ = master.read_to_end(&mut seen);
        seen
    });
    let exited = shux_testkit::poll(shux_testkit::BUDGET, || child.try_wait().ok().flatten());
    let status = match exited {
        Some(status) => status,
        None => {
            let _ = kill(Pid::from_raw(child.id() as i32), Signal::SIGKILL);
            child.wait().expect("reap attach")
        }
    };
    assert!(
        shux_testkit::poll_until(shux_testkit::BUDGET, || reader.is_finished()),
        "the PTY master never reached EOF after attach exited"
    );
    let seen = reader.join().expect("pty reader");
    (status.code(), text(&seen))
}
