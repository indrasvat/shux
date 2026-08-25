//! Real-daemon lifecycle regression tests.
//!
//! These tests use the built shux binary with an isolated XDG_RUNTIME_DIR,
//! create real PTY children, then assert pane/window/session/daemon teardown
//! reaps those children. The bug this protects against: the graph entry was
//! removed, but the interactive TUI process stayed alive under the daemon.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill, killpg};
use nix::unistd::Pid;
use shux_rpc::attach::{
    ATTACH_PROTOCOL_VERSION, AttachClientFrame, AttachHello, AttachReady, AttachServerFrame,
};
use tokio::net::UnixStream;
use tokio_util::codec::Framed;

struct ShuxHarness {
    bin: PathBuf,
    runtime: tempfile::TempDir,
}

impl ShuxHarness {
    fn new() -> Self {
        Self {
            bin: PathBuf::from(env!("CARGO_BIN_EXE_shux")),
            runtime: tempfile::tempdir().expect("temp runtime dir"),
        }
    }

    fn runtime_dir(&self) -> &Path {
        self.runtime.path()
    }

    fn shux(&self) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.env("XDG_RUNTIME_DIR", self.runtime_dir())
            .env("NO_COLOR", "1")
            .env("CLICOLOR", "0")
            .env("SHELL", "/bin/sh");
        cmd
    }

    fn rpc(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let params = params.to_string();
        let output = self
            .shux()
            .args([
                "--format", "json", "rpc", "call", method, "--params", &params,
            ])
            .output()
            .unwrap_or_else(|e| panic!("failed to run shux rpc {method}: {e}"));
        parse_rpc_output(method, output)
    }

    fn create_stubborn_session(
        &self,
        name: &str,
        cwd: &Path,
        pid_file: &Path,
    ) -> serde_json::Value {
        let result = self.rpc(
            "session.create",
            serde_json::json!({
                "name": name,
                "cwd": cwd.display().to_string(),
                "command": stubborn_command(pid_file),
            }),
        );
        wait_for_pid_file(pid_file);
        result
    }

    fn daemon_pid(&self) -> Option<u32> {
        let path = self.runtime_dir().join("shux").join("shux.pid");
        std::fs::read_to_string(path).ok()?.trim().parse().ok()
    }

    fn terminate_daemon(&self) {
        if let Some(pid) = self.daemon_pid() {
            let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
            wait_for_pid_gone(pid, Duration::from_secs(5));
        }
    }
}

impl Drop for ShuxHarness {
    fn drop(&mut self) {
        self.terminate_daemon();
    }
}

fn parse_rpc_output(method: &str, output: Output) -> serde_json::Value {
    if !output.status.success() {
        panic!(
            "shux rpc {method} failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "invalid JSON from shux rpc {method}: {e}\nstdout:\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    if let Some(error) = envelope.get("error") {
        panic!("shux rpc {method} returned error: {error}");
    }
    envelope
        .get("result")
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

fn stubborn_command(pid_file: &Path) -> Vec<String> {
    vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!(
            "echo $$ > {}; trap '' HUP TERM INT; while :; do sleep 1; done",
            shell_quote(pid_file)
        ),
    ]
}

fn try_read_pid(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn wait_for_pid_file(path: &Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(pid) = try_read_pid(path)
            && pid_exists(pid)
        {
            return pid;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "pid file {} did not appear with a live process",
        path.display()
    );
}

fn pid_exists(pid: u32) -> bool {
    match kill(Pid::from_raw(pid as i32), None) {
        Ok(()) => true,
        Err(Errno::ESRCH) => false,
        Err(_) => true,
    }
}

fn wait_for_pid_gone(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !pid_exists(pid) {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

fn assert_pid_gone(pid: u32, context: &str) {
    if wait_for_pid_gone(pid, Duration::from_secs(5)) {
        return;
    }
    let _ = killpg(Pid::from_raw(pid as i32), Signal::SIGKILL);
    let _ = kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
    panic!("{context}: pid {pid} was still alive after teardown");
}

#[test]
fn session_kill_reaps_multiple_stubborn_sessions() {
    let h = ShuxHarness::new();
    let work = tempfile::tempdir().expect("work dir");
    let mut pids = Vec::new();

    for idx in 0..4 {
        let pid_file = work.path().join(format!("session-{idx}.pid"));
        h.create_stubborn_session(&format!("life-session-{idx}"), work.path(), &pid_file);
        pids.push((format!("life-session-{idx}"), wait_for_pid_file(&pid_file)));
    }

    for (name, _) in &pids {
        h.rpc("session.kill", serde_json::json!({ "name": name }));
    }

    for (name, pid) in pids {
        assert_pid_gone(pid, &format!("session.kill {name} should reap child"));
    }

    let sessions = h.rpc("session.list", serde_json::json!({}));
    assert_eq!(
        sessions["sessions"].as_array().map(Vec::len),
        Some(0),
        "all killed sessions should be gone from the graph"
    );
}

#[test]
fn window_kill_reaps_only_that_windows_pane_child() {
    let h = ShuxHarness::new();
    let work = tempfile::tempdir().expect("work dir");
    let first_pid_file = work.path().join("first.pid");
    let second_pid_file = work.path().join("second.pid");

    let session = h.create_stubborn_session("life-window", work.path(), &first_pid_file);
    let session_id = session["id"].as_str().expect("session id");
    let first_pid = wait_for_pid_file(&first_pid_file);

    let window = h.rpc(
        "window.create",
        serde_json::json!({
            "session_id": session_id,
            "name": "extra",
            "cwd": work.path().display().to_string(),
            "command": stubborn_command(&second_pid_file),
        }),
    );
    let window_id = window["id"].as_str().expect("window id");
    let second_pid = wait_for_pid_file(&second_pid_file);

    h.rpc("window.kill", serde_json::json!({ "id": window_id }));

    assert_pid_gone(second_pid, "window.kill should reap killed window child");
    assert!(
        pid_exists(first_pid),
        "window.kill must not reap sibling windows in the same session"
    );

    h.rpc("session.kill", serde_json::json!({ "name": "life-window" }));
    assert_pid_gone(first_pid, "session.kill should reap surviving window child");
}

#[test]
fn pane_kill_reaps_only_that_pane_child() {
    let h = ShuxHarness::new();
    let work = tempfile::tempdir().expect("work dir");
    let first_pid_file = work.path().join("first-pane.pid");
    let second_pid_file = work.path().join("second-pane.pid");

    let session = h.create_stubborn_session("life-pane", work.path(), &first_pid_file);
    let first_pane_id = session["pane_id"].as_str().expect("pane id");
    let first_pid = wait_for_pid_file(&first_pid_file);

    let split = h.rpc(
        "pane.split",
        serde_json::json!({
            "pane_id": first_pane_id,
            "direction": "vertical",
            "ratio": 0.5,
            "cwd": work.path().display().to_string(),
            "command": stubborn_command(&second_pid_file),
        }),
    );
    let second_pane_id = split["pane"]["id"].as_str().expect("split pane id");
    let second_pid = wait_for_pid_file(&second_pid_file);

    h.rpc(
        "pane.kill",
        serde_json::json!({ "pane_id": second_pane_id }),
    );

    assert_pid_gone(second_pid, "pane.kill should reap killed pane child");
    assert!(
        pid_exists(first_pid),
        "pane.kill must not reap sibling panes in the same window"
    );

    h.rpc("session.kill", serde_json::json!({ "name": "life-pane" }));
    assert_pid_gone(first_pid, "session.kill should reap surviving pane child");
}

#[test]
fn daemon_shutdown_reaps_all_live_pane_children() {
    let h = ShuxHarness::new();
    let work = tempfile::tempdir().expect("work dir");
    let mut pids = Vec::new();

    for idx in 0..3 {
        let pid_file = work.path().join(format!("daemon-{idx}.pid"));
        h.create_stubborn_session(&format!("life-daemon-{idx}"), work.path(), &pid_file);
        pids.push(wait_for_pid_file(&pid_file));
    }

    h.terminate_daemon();

    for pid in pids {
        assert_pid_gone(pid, "daemon SIGTERM should reap every live pane child");
    }
}

#[tokio::test]
async fn attach_detach_does_not_orphan_or_kill_pane_child() {
    let h = ShuxHarness::new();
    let work = tempfile::tempdir().expect("work dir");
    let pid_file = work.path().join("attach.pid");
    let session = h.create_stubborn_session("life-attach", work.path(), &pid_file);
    let pid = wait_for_pid_file(&pid_file);
    let session_id = session["id"].as_str().expect("session id").to_string();

    let attach_path = h.runtime_dir().join("shux").join("attach.sock");
    let stream = UnixStream::connect(&attach_path)
        .await
        .expect("connect attach socket");
    let mut framed = Framed::new(stream, shux_rpc::create_codec());
    let hello = AttachHello {
        protocol: ATTACH_PROTOCOL_VERSION,
        session_name: Some("life-attach".to_string()),
        cols: 100,
        rows: 30,
        client_version: "test".to_string(),
    };
    framed
        .send(Bytes::from(serde_json::to_vec(&hello).expect("hello JSON")))
        .await
        .expect("send attach hello");

    let ready = framed
        .next()
        .await
        .expect("attach ready frame")
        .expect("attach ready bytes");
    let ready: AttachReady = serde_json::from_slice(&ready).expect("parse attach ready");
    match ready {
        AttachReady::Ok {
            session_id: sid, ..
        } => assert_eq!(sid, session_id),
        AttachReady::Error { code, message } => panic!("attach failed: {code}: {message}"),
    }

    let detach = AttachClientFrame::Detach;
    framed
        .send(Bytes::from(
            serde_json::to_vec(&detach).expect("detach JSON"),
        ))
        .await
        .expect("send detach");

    loop {
        let frame = framed
            .next()
            .await
            .expect("detach ack frame")
            .expect("detach ack bytes");
        let frame: AttachServerFrame = serde_json::from_slice(&frame).expect("server frame");
        if matches!(frame, AttachServerFrame::DetachAck) {
            break;
        }
    }

    assert!(
        pid_exists(pid),
        "detach should leave the pane child running"
    );
    h.rpc("session.kill", serde_json::json!({ "name": "life-attach" }));
    assert_pid_gone(pid, "session.kill after detach should reap pane child");
}

/// `daemon stop` must stop the daemon it started, whatever the binary is called.
///
/// The identity check that keeps `daemon stop` from signalling a bystander used
/// to require the executable's basename to be literally `shux`. Any other name —
/// an A/B pair like `shux-BEFORE`/`shux-AFTER`, a versioned or distro-renamed
/// install — made shux fail to recognise its OWN daemon. Two things then went
/// wrong at once: it printed "no daemon running" and exited 0, so a cleanup trap
/// believed the daemon was gone, and it deleted the pidfile, orphaning a live
/// daemon that nothing could find afterwards. Every A/B harness leaked one
/// daemon per run.
#[test]
fn daemon_stop_reaps_a_daemon_started_by_a_renamed_binary() {
    let runtime = tempfile::tempdir().expect("temp runtime dir");
    let bin_dir = tempfile::tempdir().expect("temp bin dir");
    let renamed = bin_dir.path().join("shux-under-a-different-name");
    std::fs::copy(env!("CARGO_BIN_EXE_shux"), &renamed).expect("copy shux binary");
    let mut perms = std::fs::metadata(&renamed).expect("stat").permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
    }
    std::fs::set_permissions(&renamed, perms).expect("chmod");

    let run = |args: &[&str]| -> Output {
        Command::new(&renamed)
            .args(args)
            .env("XDG_RUNTIME_DIR", runtime.path())
            .env_remove("SHUX_SOCKET")
            .env("NO_COLOR", "1")
            .env("SHELL", "/bin/sh")
            .output()
            .expect("run renamed shux")
    };

    // Auto-starts the daemon.
    let created = run(&[
        "--format",
        "json",
        "session",
        "create",
        "renamed-daemon-test",
        "-d",
        "--",
        "sh",
        "-c",
        "sleep 120",
    ]);
    assert!(
        created.status.success(),
        "session create failed: {}",
        String::from_utf8_lossy(&created.stderr)
    );

    // A test for a leak bug that leaks when it fails is its own bug: an earlier
    // draft asserted first and reaped last, and left a daemon behind every time
    // it ran against the unfixed code. The guard is installed before the pidfile
    // is read and adopts the pid the moment it parses, so every path from there
    // on reaps -- including the `daemon stop` failure and daemon-survived paths,
    // which are the ones that actually fire.
    //
    // What it does NOT cover, deliberately: a missing or unparseable pidfile
    // still panics with `self.0 == None`. There is no pid to reap in that case,
    // so there is nothing the guard could do; the daemon, if any, is
    // unreachable by pid either way.
    struct Reaper(Option<i32>);
    impl Drop for Reaper {
        fn drop(&mut self) {
            if let Some(pid) = self.0
                && kill(Pid::from_raw(pid), None).is_ok()
            {
                let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
            }
        }
    }
    let mut reaper = Reaper(None);

    let pid_path = runtime.path().join("shux").join("shux.pid");
    let pid_text = std::fs::read_to_string(&pid_path).expect("pidfile");
    // Adopt the pid the instant it is known, before anything can panic on it.
    reaper.0 = pid_text.trim().parse().ok();
    let pid: i32 = reaper
        .0
        .unwrap_or_else(|| panic!("unparseable pid {pid_text:?}"));
    assert!(
        kill(Pid::from_raw(pid), None).is_ok(),
        "daemon {pid} should be alive before stop"
    );

    let _ = run(&["session", "kill", "renamed-daemon-test"]);
    let stopped = run(&["daemon", "stop"]);
    assert!(
        stopped.status.success(),
        "daemon stop failed: {}",
        String::from_utf8_lossy(&stopped.stderr)
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if kill(Pid::from_raw(pid), None).is_err() {
            return; // reaped
        }
        thread::sleep(Duration::from_millis(50));
    }

    panic!(
        "daemon {pid} survived `daemon stop`, which reported: {}",
        String::from_utf8_lossy(&stopped.stdout).trim()
    );
}

// ── pidfile identity: three defects, one root cause ─────────────────────────
//
// The pidfile is untrusted input -- it survives SIGKILL and reboots, and pids
// get reused -- so every one of these is about who a pid actually belongs to.
// Each test below was seen RED against the code before its fix.

/// Reaps an adopted pid on drop, so a test for a leak never leaks when it fails.
struct Reaper(Vec<i32>);
impl Drop for Reaper {
    fn drop(&mut self) {
        for pid in &self.0 {
            if kill(Pid::from_raw(*pid), None).is_ok() {
                let _ = kill(Pid::from_raw(*pid), Signal::SIGKILL);
            }
        }
    }
}

fn copy_bin(dir: &Path, name: &str) -> PathBuf {
    let dst = dir.join(name);
    std::fs::copy(env!("CARGO_BIN_EXE_shux"), &dst).expect("copy shux binary");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&dst).expect("stat").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dst, perms).expect("chmod");
    }
    dst
}

/// Poll until `dir` holds `n` pidfiles naming live processes.
///
/// A fixed sleep is not good enough here: daemon startup under the coverage job's
/// instrumentation, or on a loaded macOS runner, routinely outruns any constant
/// small enough to keep the suite quick. This mirrors `wait_for_pid_file`.
fn wait_for_pid_files(dir: &Path, n: usize) -> Vec<u32> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let found: Vec<u32> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "pid"))
            .filter_map(|e| try_read_pid(&e.path()))
            .filter(|pid| pid_exists(*pid))
            .collect();
        if found.len() >= n {
            return found;
        }
        if Instant::now() >= deadline {
            panic!(
                "{} held {} live pidfile(s), expected {n}",
                dir.display(),
                found.len()
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn run_bin(bin: &Path, runtime: &Path, socket: Option<&Path>, args: &[&str]) -> Output {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .env("XDG_RUNTIME_DIR", runtime)
        .env("NO_COLOR", "1")
        .env("SHELL", "/bin/sh");
    match socket {
        Some(s) => cmd.env("SHUX_SOCKET", s),
        None => cmd.env_remove("SHUX_SOCKET"),
    };
    cmd.output().expect("run shux")
}

/// One build must stop a daemon another build started.
///
/// Identity used to be the executable: first "basename is `shux`", then "path
/// equals `current_exe()`". Both disowned this case -- `daemon stop` printed
/// "no daemon running", exited 0, deleted the pidfile and left the daemon alive.
/// A daemon is ours because of the socket it serves, not what the file is named.
#[test]
fn one_build_stops_a_daemon_another_build_started() {
    let runtime = tempfile::tempdir().expect("temp runtime");
    let bins = tempfile::tempdir().expect("temp bins");
    let starter = copy_bin(bins.path(), "shux-AAA");
    let stopper = copy_bin(bins.path(), "shux-BBB");
    let mut reaper = Reaper(Vec::new());

    let created = run_bin(&starter, runtime.path(), None, &["session", "list"]);
    assert!(created.status.success(), "session list failed");

    let pid = wait_for_pid_file(&runtime.path().join("shux").join("shux.pid"));
    reaper.0.push(pid as i32);

    let stopped = run_bin(&stopper, runtime.path(), None, &["daemon", "stop"]);
    assert!(stopped.status.success(), "daemon stop failed");
    assert!(
        wait_for_pid_gone(pid, Duration::from_secs(5)),
        "daemon {pid} started by shux-AAA survived `shux-BBB daemon stop`: {}",
        String::from_utf8_lossy(&stopped.stdout)
    );
}

/// `daemon stop` must never signal a daemon serving a different socket.
///
/// Identity ignored which daemon it had found, so any shux daemon at that pid
/// qualified. On a recycled pid that is another checkout's daemon, killed by a
/// routine cleanup trap. Here the wrong pid is planted directly, which is what a
/// pid collision looks like from the code's point of view.
#[test]
fn daemon_stop_spares_a_daemon_serving_another_socket() {
    let theirs = tempfile::tempdir().expect("their runtime");
    let ours = tempfile::tempdir().expect("our runtime");
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_shux"));
    let mut reaper = Reaper(Vec::new());

    assert!(
        run_bin(&bin, theirs.path(), None, &["session", "list"])
            .status
            .success()
    );
    let their_pid = wait_for_pid_file(&theirs.path().join("shux").join("shux.pid"));
    reaper.0.push(their_pid as i32);

    // Our runtime dir's pidfile names THEIR daemon.
    let our_pidfile = ours.path().join("shux").join("shux.pid");
    std::fs::create_dir_all(our_pidfile.parent().unwrap()).expect("mkdir");
    std::fs::write(&our_pidfile, their_pid.to_string()).expect("plant pidfile");

    let stopped = run_bin(&bin, ours.path(), None, &["daemon", "stop"]);
    assert!(stopped.status.success(), "daemon stop must stay idempotent");

    // Two POSITIVE assertions instead of sleeping and hoping. Waiting a fixed
    // interval to see whether something died is a guess -- too short and it
    // passes on a daemon that was about to die, too long and the suite crawls.
    //
    // 1. Our invocation must say it refused. `daemon stop` has already exited,
    //    so if it were going to signal, it signalled before this returned.
    let out = String::from_utf8_lossy(&stopped.stdout);
    assert!(
        out.contains("no daemon running"),
        "expected a refusal, got: {out}"
    );
    // 2. Their daemon must still SERVE, which is stronger than still existing:
    //    a SIGTERMed daemon stops answering. A successful round-trip is a
    //    positive, immediate fact -- no waiting involved.
    let their_reply = run_bin(&bin, theirs.path(), None, &["session", "list"]);
    assert!(
        their_reply.status.success(),
        "a daemon serving another socket stopped answering after our `daemon stop`: {}",
        String::from_utf8_lossy(&their_reply.stderr)
    );
    assert!(
        pid_exists(their_pid),
        "`daemon stop` killed a daemon serving another socket (pid {their_pid})"
    );
}

/// Two daemons in one runtime dir on different sockets must both be stoppable.
///
/// The pidfile was `$RUNTIME_DIR/shux.pid` while the socket is independently
/// overridable, so the second daemon overwrote the first's entry and the first
/// became unreachable: `daemon stop` could never name it again and it ran until
/// the machine went down.
#[test]
fn two_sockets_in_one_runtime_dir_do_not_share_a_pidfile() {
    let runtime = tempfile::tempdir().expect("temp runtime");
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_shux"));
    let one = runtime.path().join("one.sock");
    let two = runtime.path().join("two.sock");
    let mut reaper = Reaper(Vec::new());

    for sock in [&one, &two] {
        assert!(
            run_bin(&bin, runtime.path(), Some(sock), &["session", "list"])
                .status
                .success(),
            "session list on {sock:?} failed"
        );
    }
    // Two daemons must own two pidfiles. Sharing one is the defect: the second
    // overwrote the first's entry and the first became unreachable.
    let pids = wait_for_pid_files(&runtime.path().join("shux"), 2);
    assert_eq!(
        pids.len(),
        2,
        "two daemons on two sockets must have two pidfiles, found {pids:?}"
    );
    for pid in &pids {
        reaper.0.push(*pid as i32);
    }

    for sock in [&one, &two] {
        let out = run_bin(&bin, runtime.path(), Some(sock), &["daemon", "stop"]);
        assert!(out.status.success(), "daemon stop on {sock:?} failed");
    }
    for pid in &pids {
        assert!(
            wait_for_pid_gone(*pid, Duration::from_secs(5)),
            "daemon {pid} was orphaned -- its pidfile was overwritten by the other socket"
        );
    }
}

/// Upgrading must not orphan a daemon started by the previous version.
///
/// Every shux before the socket-keyed pidfile wrote `$RUNTIME_DIR/shux.pid`
/// whatever socket it served. A client that looked only at the new hashed name
/// could not see such a daemon: it reported "no daemon running", left it alive
/// and unreachable, and rebound its socket underneath it -- this PR's own bug,
/// reintroduced at every upgrade. Simulated by moving the pidfile to where the
/// old version would have written it, which is exactly the state upgrading
/// leaves behind.
#[test]
fn a_daemon_from_before_the_socket_keyed_pidfile_is_still_stoppable() {
    let runtime = tempfile::tempdir().expect("temp runtime");
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_shux"));
    let sock = runtime.path().join("custom.sock");
    let mut reaper = Reaper(Vec::new());

    assert!(
        run_bin(&bin, runtime.path(), Some(&sock), &["session", "list"])
            .status
            .success()
    );
    let pids = wait_for_pid_files(&runtime.path().join("shux"), 1);
    let pid = pids[0];
    reaper.0.push(pid as i32);

    // Rewrite history: put the pid where the PREVIOUS version would have.
    let hashed = std::fs::read_dir(runtime.path().join("shux"))
        .expect("runtime dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "pid"))
        .expect("a pidfile");
    let legacy = runtime.path().join("shux").join("shux.pid");
    std::fs::rename(&hashed, &legacy).expect("move pidfile to the legacy path");

    let stopped = run_bin(&bin, runtime.path(), Some(&sock), &["daemon", "stop"]);
    assert!(stopped.status.success(), "daemon stop failed");
    assert!(
        wait_for_pid_gone(pid, Duration::from_secs(5)),
        "a daemon recorded at the pre-upgrade pidfile path was orphaned: {}",
        String::from_utf8_lossy(&stopped.stdout)
    );
}

/// Reading the legacy pidfile must not let one daemon claim another.
///
/// The migration above consults `$RUNTIME_DIR/shux.pid` when the socket-keyed
/// file is absent. That file may belong to the DEFAULT daemon, so the identity
/// check is what keeps the migration safe: a custom-socket client must not stop
/// the default daemon just because its pid is the one recorded there.
#[test]
fn the_legacy_pidfile_is_not_claimed_by_a_daemon_serving_another_socket() {
    let runtime = tempfile::tempdir().expect("temp runtime");
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_shux"));
    let mut reaper = Reaper(Vec::new());

    // A DEFAULT-socket daemon, whose pidfile is the legacy path by definition.
    assert!(
        run_bin(&bin, runtime.path(), None, &["session", "list"])
            .status
            .success()
    );
    let pid = wait_for_pid_file(&runtime.path().join("shux").join("shux.pid"));
    reaper.0.push(pid as i32);

    let other = runtime.path().join("other.sock");
    let stopped = run_bin(&bin, runtime.path(), Some(&other), &["daemon", "stop"]);
    assert!(stopped.status.success(), "daemon stop must stay idempotent");
    assert!(
        String::from_utf8_lossy(&stopped.stdout).contains("no daemon running"),
        "expected a refusal for a socket no daemon serves"
    );
    // Still serving is stronger than still existing.
    assert!(
        run_bin(&bin, runtime.path(), None, &["session", "list"])
            .status
            .success(),
        "the default daemon was stopped by a client asking about another socket"
    );
}
