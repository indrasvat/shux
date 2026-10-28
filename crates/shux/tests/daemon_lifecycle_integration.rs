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
        graphics: false,
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

// The pidfile is untrusted: it survives SIGKILL and reboots and pids get reused,
// so each test below is about who a pid actually belongs to. Each was seen RED.

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
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&dst).expect("stat").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&dst, perms).expect("chmod");
    dst
}

/// Poll until `dir` holds `n` pidfiles naming live processes.
///
/// A fixed sleep is not good enough here: daemon startup under the coverage job's
/// instrumentation, or on a loaded macOS runner, routinely outruns any constant
/// small enough to keep the suite quick. This mirrors `wait_for_pid_file`.
fn wait_for_pid_files(dir: &Path, n: usize) -> Vec<(PathBuf, u32)> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let found: Vec<(PathBuf, u32)> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "pid"))
            .filter_map(|p| try_read_pid(&p).map(|pid| (p, pid)))
            .filter(|(_, pid)| pid_exists(*pid))
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

fn bin_cmd(bin: &Path, runtime: &Path, socket: Option<&Path>, args: &[&str]) -> Command {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .env("XDG_RUNTIME_DIR", runtime)
        .env("NO_COLOR", "1")
        .env("SHELL", "/bin/sh");
    match socket {
        Some(s) => cmd.env("SHUX_SOCKET", s),
        None => cmd.env_remove("SHUX_SOCKET"),
    };
    cmd
}

fn run_bin(bin: &Path, runtime: &Path, socket: Option<&Path>, args: &[&str]) -> Output {
    bin_cmd(bin, runtime, socket, args)
        .output()
        .expect("run shux")
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

    // Two positive assertions rather than sleeping to see whether something
    // died: `daemon stop` has already exited, so it signalled or it did not.
    let out = String::from_utf8_lossy(&stopped.stdout);
    assert!(
        out.contains("no daemon running"),
        "expected a refusal, got: {out}"
    );
    // Still SERVING is stronger than still existing.
    let their_reply = run_bin(&bin, theirs.path(), None, &["session", "list"]);
    assert!(
        their_reply.status.success(),
        "a daemon serving another socket stopped answering after our `daemon stop`: {}",
        String::from_utf8_lossy(&their_reply.stderr)
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
    let pids: Vec<u32> = wait_for_pid_files(&runtime.path().join("shux"), 2)
        .into_iter()
        .map(|(_, pid)| pid)
        .collect();
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
    let (hashed, pid) = wait_for_pid_files(&runtime.path().join("shux"), 1)
        .into_iter()
        .next()
        .expect("a pidfile");
    reaper.0.push(pid as i32);

    // Rewrite history: put the pid where the PREVIOUS version would have.
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

/// Pids of every daemon serving `socket`, by argv.
///
/// `socket` lives in a per-test tempdir, so the needle is unique to this run;
/// `ps` is a machine-wide view and a shared needle would count other suites.
fn daemons_serving(socket: &Path) -> Vec<i32> {
    let canonical = socket
        .parent()
        .and_then(|dir| dir.canonicalize().ok())
        .zip(socket.file_name())
        .map_or_else(|| socket.to_path_buf(), |(dir, name)| dir.join(name));
    let needles =
        [socket, canonical.as_path()].map(|s| format!("__daemon --socket {}", s.display()));
    let out = Command::new("ps")
        .args(["-ww", "-Ao", "pid=,command="])
        .output()
        .expect("run ps");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| needles.iter().any(|n| line.trim_end().ends_with(n)))
        .filter_map(|line| line.split_whitespace().next()?.parse().ok())
        .collect()
}

/// Poll until exactly `n` daemons serve `socket`, returning the last count seen.
fn settle_daemon_count(socket: &Path, n: usize, reaper: &mut Reaper) -> usize {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let pids = daemons_serving(socket);
        for pid in &pids {
            if !reaper.0.contains(pid) {
                reaper.0.push(*pid);
            }
        }
        if pids.len() == n || Instant::now() >= deadline {
            return pids.len();
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Concurrent first-use clients must start one daemon, and `daemon stop` must
/// leave none (#193).
#[test]
fn concurrent_first_use_clients_start_exactly_one_daemon() {
    let runtime = tempfile::tempdir().expect("temp runtime");
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_shux"));
    let socket = runtime.path().join("shux").join("shux.sock");
    let mut reaper = Reaper(Vec::new());

    let clients: Vec<_> = (0..8)
        .map(|_| {
            bin_cmd(&bin, runtime.path(), None, &["session", "list"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("spawn shux")
        })
        .collect();
    for client in clients {
        let out = client.wait_with_output().expect("wait for shux");
        assert!(
            out.status.success(),
            "a concurrent first-use client failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let running = settle_daemon_count(&socket, 1, &mut reaper);
    assert_eq!(running, 1, "{running} daemons serve one socket");

    let stopped = run_bin(&bin, runtime.path(), None, &["daemon", "stop"]);
    assert!(stopped.status.success(), "daemon stop failed");
    let left = settle_daemon_count(&socket, 0, &mut reaper);
    assert_eq!(left, 0, "{left} daemon(s) survived `daemon stop`");
}

/// A daemon started on a socket another daemon already serves must exit, not
/// steal the socket and strand the first one (#193).
#[test]
fn a_second_daemon_on_a_served_socket_exits() {
    let runtime = tempfile::tempdir().expect("temp runtime");
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_shux"));
    let socket = runtime.path().join("shux").join("shux.sock");
    let mut reaper = Reaper(Vec::new());

    assert!(
        run_bin(&bin, runtime.path(), None, &["session", "list"])
            .status
            .success()
    );
    let first = wait_for_pid_file(&runtime.path().join("shux").join("shux.pid"));
    reaper.0.push(first as i32);

    let late = run_bin(
        &bin,
        runtime.path(),
        None,
        &[
            "__daemon",
            "--socket",
            socket.to_str().expect("utf-8 socket"),
        ],
    );
    assert!(late.status.success(), "__daemon parent failed");

    let running = settle_daemon_count(&socket, 1, &mut reaper);
    assert_eq!(running, 1, "{running} daemons serve one socket");
    assert_eq!(
        daemons_serving(&socket),
        vec![first as i32],
        "the late daemon replaced the one already serving"
    );
    assert_eq!(
        try_read_pid(&runtime.path().join("shux").join("shux.pid")),
        Some(first),
        "the late daemon overwrote the pidfile"
    );
    assert!(
        run_bin(&bin, runtime.path(), None, &["session", "list"])
            .status
            .success(),
        "the first daemon stopped answering"
    );

    let stopped = run_bin(&bin, runtime.path(), None, &["daemon", "stop"]);
    assert!(stopped.status.success(), "daemon stop failed");
    assert!(
        wait_for_pid_gone(first, Duration::from_secs(5)),
        "daemon stop left the first daemon running"
    );
}

/// A daemon must not take over a socket a server without the instance lock is
/// already serving — a daemon from before the lock existed, at an upgrade.
#[test]
fn a_daemon_leaves_a_socket_a_lockless_server_already_serves() {
    let runtime = tempfile::tempdir().expect("temp runtime");
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_shux"));
    let dir = runtime.path().join("shux");
    std::fs::create_dir_all(&dir).expect("mkdir runtime");
    let socket = dir.join("shux.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind fake server");
    listener.set_nonblocking(true).expect("nonblocking");
    let mut reaper = Reaper(Vec::new());

    let late = run_bin(
        &bin,
        runtime.path(),
        None,
        &[
            "__daemon",
            "--socket",
            socket.to_str().expect("utf-8 socket"),
        ],
    );
    assert!(late.status.success(), "__daemon parent failed");

    // The daemon's probe connects here before it gives up; waiting for it means
    // the count below cannot read zero merely because it has not started yet.
    let deadline = Instant::now() + Duration::from_secs(10);
    while listener.accept().is_err() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    let running = settle_daemon_count(&socket, 0, &mut reaper);
    assert_eq!(running, 0, "a daemon is running beside the server");
    assert!(
        std::os::unix::net::UnixStream::connect(&socket).is_ok() && listener.accept().is_ok(),
        "the daemon replaced the socket the server was listening on"
    );
    assert!(
        !dir.join("shux.pid").exists(),
        "a daemon that did not start wrote the pidfile"
    );
}

/// A client that arrives while the daemon drains its panes must get a fresh
/// daemon once the old one exits, not a spawn that loses to the departing
/// daemon's instance lock and a connect that times out.
#[test]
fn a_client_during_a_daemon_drain_gets_the_next_daemon() {
    let h = ShuxHarness::new();
    let socket = h.runtime_dir().join("shux").join("shux.sock");
    let work = tempfile::tempdir().expect("work dir");
    let pane_pidfile = work.path().join("pane.pid");
    let mut reaper = Reaper(Vec::new());

    // Ignores SIGTERM and SIGHUP, so the daemon drains for its full budget.
    h.create_stubborn_session("drain", work.path(), &pane_pidfile);
    let pane = wait_for_pid_file(&pane_pidfile);
    let old = h.daemon_pid().expect("daemon pidfile");
    reaper.0.extend([old as i32, pane as i32]);

    kill(Pid::from_raw(old as i32), Signal::SIGTERM).expect("SIGTERM daemon");
    let deadline = Instant::now() + Duration::from_secs(5);
    while socket.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !socket.exists() && pid_exists(old),
        "precondition: the daemon has stopped serving but is still draining"
    );

    let out = h
        .shux()
        .args(["session", "list"])
        .output()
        .expect("run shux");
    assert!(
        out.status.success(),
        "a client during the drain failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_pid_gone(old, "the draining daemon");
    let running = settle_daemon_count(&socket, 1, &mut reaper);
    assert_eq!(running, 1, "{running} daemons after the drain");
    assert_pid_gone(pane, "the drained daemon's pane");
}

/// A daemon whose pidfile was lost is still the daemon for its socket: `daemon
/// stop` must stop it rather than report "no daemon running" and leave it.
#[test]
fn daemon_stop_finds_a_daemon_whose_pidfile_was_lost() {
    let runtime = tempfile::tempdir().expect("temp runtime");
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_shux"));
    let pidfile = runtime.path().join("shux").join("shux.pid");
    let mut reaper = Reaper(Vec::new());

    assert!(
        run_bin(&bin, runtime.path(), None, &["session", "list"])
            .status
            .success()
    );
    let pid = wait_for_pid_file(&pidfile);
    reaper.0.push(pid as i32);
    std::fs::remove_file(&pidfile).expect("lose the pidfile");

    let stopped = run_bin(&bin, runtime.path(), None, &["daemon", "stop"]);
    assert!(stopped.status.success(), "daemon stop failed");
    assert!(
        wait_for_pid_gone(pid, Duration::from_secs(5)),
        "daemon stop left a daemon it could not name: {}",
        String::from_utf8_lossy(&stopped.stdout)
    );
}

/// A client must not take a socket from a server it cannot identify and cannot
/// stop: unlinking it strands that server, still running and unreachable.
#[test]
fn a_client_does_not_take_the_socket_of_a_server_it_cannot_stop() {
    let runtime = tempfile::tempdir().expect("temp runtime");
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_shux"));
    let dir = runtime.path().join("shux");
    std::fs::create_dir_all(&dir).expect("mkdir runtime");
    let socket = dir.join("shux.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind fake server");
    // Accepts and hangs up: the client cannot read a version, so it is stale,
    // and no pidfile names it. Detached: it dies with the test process.
    thread::spawn(move || {
        for conn in listener.incoming() {
            match conn {
                Ok(conn) => drop(conn),
                Err(_) => break,
            }
        }
    });
    let mut reaper = Reaper(Vec::new());

    let out = run_bin(&bin, runtime.path(), None, &["session", "list"]);
    assert!(
        !out.status.success(),
        "a client replaced a server it could not stop"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no shux daemon process serves it"),
        "the error must say why: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let running = settle_daemon_count(&socket, 0, &mut reaper);
    assert_eq!(running, 0, "a daemon was started beside the server");
    assert!(
        std::os::unix::net::UnixStream::connect(&socket).is_ok(),
        "the server's socket was unlinked"
    );
}

/// `daemon stop` must not unlink files once its daemon has exited: a client that
/// arrived during the stop may already have started the successor they belong to.
#[test]
fn daemon_stop_leaves_the_successors_files_alone() {
    let h = ShuxHarness::new();
    let socket = h.runtime_dir().join("shux").join("shux.sock");
    let mut reaper = Reaper(Vec::new());

    assert!(
        h.shux()
            .args(["session", "create", "k", "-d", "--", "sh", "-c", "sleep 60"])
            .output()
            .expect("run shux")
            .status
            .success()
    );
    let old = h.daemon_pid().expect("daemon pidfile");
    reaper.0.push(old as i32);

    let stop = h
        .shux()
        .args(["daemon", "stop"])
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("spawn daemon stop");
    thread::sleep(Duration::from_millis(50));
    let during = h
        .shux()
        .args(["session", "list"])
        .output()
        .expect("run shux");
    assert!(during.status.success(), "a client during the stop failed");
    stop.wait_with_output().expect("wait for daemon stop");
    assert_pid_gone(old, "the stopped daemon");

    assert_eq!(settle_daemon_count(&socket, 1, &mut reaper), 1);
    assert!(socket.exists(), "the successor's socket was unlinked");
    let started = Instant::now();
    let after = h
        .shux()
        .args(["session", "list"])
        .output()
        .expect("run shux");
    assert!(
        after.status.success() && started.elapsed() < Duration::from_secs(2),
        "the successor stopped answering ({:?}): {}",
        started.elapsed(),
        String::from_utf8_lossy(&after.stderr)
    );
}

/// Every spelling of one socket path names one daemon: concurrent clients
/// spelling it differently must still start one, and any spelling stops it.
#[test]
fn spellings_of_one_socket_share_one_daemon() {
    let runtime = tempfile::tempdir().expect("temp runtime");
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_shux"));
    let dir = runtime.path().join("s");
    std::fs::create_dir(&dir).expect("mkdir socket dir");
    let socket = dir.join("a.sock");
    let spellings = [
        socket.clone(),
        PathBuf::from(format!("{}//a.sock", dir.display())),
        dir.join("..").join("s").join("a.sock"),
    ];
    let mut reaper = Reaper(Vec::new());

    let clients: Vec<_> = (0..12)
        .map(|i| {
            bin_cmd(
                &bin,
                runtime.path(),
                Some(&spellings[i % spellings.len()]),
                &["session", "list"],
            )
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn shux")
        })
        .collect();
    for client in clients {
        let out = client.wait_with_output().expect("wait for shux");
        assert!(
            out.status.success(),
            "a client failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    assert_eq!(
        settle_daemon_count(&socket, 1, &mut reaper),
        1,
        "spellings of one socket started several daemons"
    );

    let stopped = run_bin(
        &bin,
        runtime.path(),
        Some(&spellings[2]),
        &["daemon", "stop"],
    );
    assert!(
        String::from_utf8_lossy(&stopped.stdout).contains("daemon stopped"),
        "a different spelling could not stop the daemon: {}",
        String::from_utf8_lossy(&stopped.stdout)
    );
}

/// A daemon started from a relative `SHUX_SOCKET` is found by its absolute
/// path from another cwd: a relative spelling in its argv would resolve
/// against the checker's cwd, not the one it was started from.
#[test]
fn a_daemon_started_from_a_relative_socket_is_stopped_by_its_absolute_path() {
    let runtime = tempfile::tempdir().expect("temp runtime");
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_shux"));
    let dir = runtime.path().join("d");
    std::fs::create_dir(&dir).expect("mkdir socket dir");
    let mut reaper = Reaper(Vec::new());

    let started = bin_cmd(
        &bin,
        runtime.path(),
        Some(Path::new("rel.sock")),
        &["session", "list"],
    )
    .current_dir(&dir)
    .output()
    .expect("run shux");
    assert!(
        started.status.success(),
        "a relative socket did not start a daemon: {}",
        String::from_utf8_lossy(&started.stderr)
    );
    let (_, pid) = wait_for_pid_files(&runtime.path().join("shux"), 1)
        .into_iter()
        .next()
        .expect("one pidfile");
    reaper.0.push(pid as i32);

    let stopped = bin_cmd(
        &bin,
        runtime.path(),
        Some(&dir.join("rel.sock")),
        &["daemon", "stop"],
    )
    .current_dir(runtime.path())
    .output()
    .expect("run shux");
    assert!(
        wait_for_pid_gone(pid, Duration::from_secs(5)),
        "the absolute path did not stop the daemon: {}{}",
        String::from_utf8_lossy(&stopped.stdout),
        String::from_utf8_lossy(&stopped.stderr)
    );
}

/// A daemon whose socket file was unlinked -- by a tmp cleaner, or by a daemon
/// from before the instance lock tidying up as it exits -- serves it again.
/// Otherwise it holds the instance lock, so no replacement can start either.
#[test]
fn a_daemon_whose_socket_was_unlinked_serves_it_again() {
    let h = ShuxHarness::new();
    let socket = h.runtime_dir().join("shux").join("shux.sock");
    let pidfile = h.runtime_dir().join("shux").join("shux.pid");
    let mut reaper = Reaper(Vec::new());

    assert!(
        h.shux()
            .args([
                "session", "create", "kept", "-d", "--", "sh", "-c", "sleep 60"
            ])
            .output()
            .expect("run shux")
            .status
            .success()
    );
    let pid = h.daemon_pid().expect("daemon pidfile");
    reaper.0.push(pid as i32);
    std::fs::remove_file(&socket).expect("unlink the socket");
    std::fs::remove_file(&pidfile).expect("unlink the pidfile");

    let started = Instant::now();
    let out = h
        .shux()
        .args(["session", "list"])
        .output()
        .expect("run shux");
    assert!(
        out.status.success() && started.elapsed() < Duration::from_secs(3),
        "the daemon did not serve its socket again ({:?}): {}",
        started.elapsed(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("kept"),
        "a new daemon answered instead of the one holding the session"
    );
    assert_eq!(daemons_serving(&socket), vec![pid as i32]);

    let stopped = h
        .shux()
        .args(["daemon", "stop"])
        .output()
        .expect("run shux");
    assert!(stopped.status.success(), "daemon stop failed");
    assert_pid_gone(pid, "the daemon");
}

/// A daemon that accepts connections and never answers -- stopped, or wedged --
/// is reported rather than waited on for ever.
#[test]
fn a_client_reports_a_daemon_that_does_not_answer() {
    let h = ShuxHarness::new();
    assert!(
        h.shux()
            .args(["session", "list"])
            .output()
            .expect("run shux")
            .status
            .success()
    );
    let pid = h.daemon_pid().expect("daemon pidfile");
    struct Resume(i32);
    impl Drop for Resume {
        fn drop(&mut self) {
            let _ = kill(Pid::from_raw(self.0), Signal::SIGCONT);
        }
    }
    let _resume = Resume(pid as i32);
    kill(Pid::from_raw(pid as i32), Signal::SIGSTOP).expect("SIGSTOP daemon");

    let mut client = h
        .shux()
        .args(["session", "list"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn shux");
    let deadline = Instant::now() + Duration::from_secs(15);
    while client.try_wait().expect("poll client").is_none() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }
    let hung = client.try_wait().expect("poll client").is_none();
    if hung {
        let _ = client.kill();
    }
    let out = client.wait_with_output().expect("wait for shux");
    assert!(!hung, "a client hung on a daemon that does not answer");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success()
            && stderr.contains("did not answer")
            && stderr.contains(&pid.to_string()),
        "the error must name the daemon: {stderr}"
    );
}

/// `daemon stop` judges exit by identity: a daemon that exited under a
/// subreaper that never reaps it stays a zombie, which `kill(pid, 0)` still
/// reports alive.
#[cfg(target_os = "linux")]
#[test]
fn daemon_stop_sees_a_daemon_exit_under_a_subreaper() {
    const SUBREAPER: &str = "import ctypes, subprocess, sys, time\n\
        assert ctypes.CDLL(None).prctl(36, 1, 0, 0, 0) == 0\n\
        subprocess.run(sys.argv[1:], check=True)\n\
        print('ready', flush=True)\n\
        time.sleep(120)\n";
    let h = ShuxHarness::new();
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_shux"));
    let mut subreaper = Command::new("python3")
        .args(["-c", SUBREAPER])
        .arg(&bin)
        .args(["session", "list"])
        .env("XDG_RUNTIME_DIR", h.runtime_dir())
        .env("NO_COLOR", "1")
        .env("SHELL", "/bin/sh")
        .env_remove("SHUX_SOCKET")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("python3 runs the subreaper");
    struct KillOnDrop(std::process::Child);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let stdout = subreaper.stdout.take().expect("subreaper stdout");
    let _subreaper = KillOnDrop(subreaper);
    let mut ready = String::new();
    std::io::BufRead::read_line(&mut std::io::BufReader::new(stdout), &mut ready)
        .expect("read subreaper");
    assert_eq!(
        ready.trim(),
        "ready",
        "the client under the subreaper failed"
    );
    let pid = h.daemon_pid().expect("daemon pidfile");

    let stopped = h
        .shux()
        .args(["daemon", "stop"])
        .output()
        .expect("run shux");
    assert!(
        stopped.status.success()
            && String::from_utf8_lossy(&stopped.stdout).contains("daemon stopped"),
        "daemon stop misjudged the exited daemon: {}{}",
        String::from_utf8_lossy(&stopped.stdout),
        String::from_utf8_lossy(&stopped.stderr)
    );
    // Precondition, or the test proves nothing: the daemon is a zombie, not gone.
    // Its argv reads back empty from the moment the kernel drops its memory, a
    // little before it turns Z, so stop can return while it still shows R.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut stat = String::new();
    while Instant::now() < deadline {
        stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        if stat.contains(") Z ") {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        stat.contains(") Z "),
        "the daemon was reaped after all: {stat:?}"
    );
}

/// A daemon that neither a pidfile nor the instance lock names -- one from
/// before the lock, whose pidfile was lost -- is still found by its argv.
#[test]
fn daemon_stop_finds_a_daemon_nothing_names() {
    let runtime = tempfile::tempdir().expect("temp runtime");
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_shux"));
    let dir = runtime.path().join("shux");
    let mut reaper = Reaper(Vec::new());

    assert!(
        run_bin(&bin, runtime.path(), None, &["session", "list"])
            .status
            .success()
    );
    let pid = wait_for_pid_file(&dir.join("shux.pid"));
    reaper.0.push(pid as i32);
    std::fs::remove_file(dir.join("shux.pid")).expect("lose the pidfile");
    std::fs::write(dir.join("shux.lock"), "").expect("blank the lock's pid");

    let status = run_bin(&bin, runtime.path(), None, &["daemon", "status"]);
    assert!(
        String::from_utf8_lossy(&status.stdout).contains(&format!("pid {pid}")),
        "daemon status missed the daemon: {}",
        String::from_utf8_lossy(&status.stdout)
    );
    let stopped = run_bin(&bin, runtime.path(), None, &["daemon", "stop"]);
    assert!(stopped.status.success(), "daemon stop failed");
    assert!(
        wait_for_pid_gone(pid, Duration::from_secs(5)),
        "daemon stop left a daemon nothing names: {}",
        String::from_utf8_lossy(&stopped.stdout)
    );
}

/// Daemons on two sockets in one runtime dir each keep their own attach socket:
/// the second must not replace the first's, nor delete it when it stops.
#[test]
fn two_sockets_in_one_runtime_dir_do_not_share_an_attach_socket() {
    use std::os::unix::fs::MetadataExt;
    let runtime = tempfile::tempdir().expect("temp runtime");
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_shux"));
    let dir = runtime.path().join("shux");
    let other = runtime.path().join("other.sock");
    let attach = dir.join("attach.sock");
    let mut reaper = Reaper(Vec::new());

    assert!(
        run_bin(&bin, runtime.path(), None, &["session", "list"])
            .status
            .success()
    );
    reaper
        .0
        .push(wait_for_pid_file(&dir.join("shux.pid")) as i32);
    let ino = std::fs::metadata(&attach).expect("attach socket").ino();

    assert!(
        run_bin(&bin, runtime.path(), Some(&other), &["session", "list"])
            .status
            .success()
    );
    for (_, pid) in wait_for_pid_files(&dir, 2) {
        reaper.0.push(pid as i32);
    }
    assert_eq!(
        std::fs::metadata(&attach).map(|m| m.ino()).ok(),
        Some(ino),
        "the second daemon took over the first's attach socket"
    );
    let stopped = run_bin(&bin, runtime.path(), Some(&other), &["daemon", "stop"]);
    assert!(stopped.status.success(), "daemon stop failed");
    assert!(
        std::os::unix::net::UnixStream::connect(&attach).is_ok(),
        "stopping the second daemon removed the first's attach socket"
    );

    let stopped = run_bin(&bin, runtime.path(), None, &["daemon", "stop"]);
    assert!(stopped.status.success(), "daemon stop failed");
}

/// A spelling with `..` after a directory that does not exist yet reaches the
/// daemon it starts: the kernel cannot resolve `x/..` until `x` exists.
#[test]
fn a_socket_spelled_through_a_missing_directory_is_reached() {
    let runtime = tempfile::tempdir().expect("temp runtime");
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_shux"));
    let socket = runtime.path().join("x").join("..").join("a.sock");
    let mut reaper = Reaper(Vec::new());

    let out = run_bin(&bin, runtime.path(), Some(&socket), &["session", "list"]);
    for (_, pid) in wait_for_pid_files(&runtime.path().join("shux"), 1) {
        reaper.0.push(pid as i32);
    }
    assert!(
        out.status.success(),
        "the client could not reach the daemon it started: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        settle_daemon_count(&runtime.path().join("a.sock"), 1, &mut reaper),
        1
    );
}

/// A relative `--socket` names the socket in the caller's directory: `daemon
/// stop` from another directory must not stop a daemon serving the same
/// relative spelling there, and from its own directory it must.
#[test]
fn a_relative_socket_names_the_callers_directory() {
    let runtime = tempfile::tempdir().expect("temp runtime");
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_shux"));
    let (a, b) = (runtime.path().join("a"), runtime.path().join("b"));
    std::fs::create_dir(&a).expect("mkdir a");
    std::fs::create_dir(&b).expect("mkdir b");
    let mut reaper = Reaper(Vec::new());

    // Spawned as an earlier build would: the relative spelling in its argv.
    let started = bin_cmd(
        &bin,
        runtime.path(),
        None,
        &["__daemon", "--socket", "rel.sock"],
    )
    .current_dir(&a)
    .status()
    .expect("start daemon");
    assert!(started.success());
    let (_, pid) = wait_for_pid_files(&runtime.path().join("shux"), 1)
        .into_iter()
        .next()
        .expect("one pidfile");
    reaper.0.push(pid as i32);

    let elsewhere = bin_cmd(
        &bin,
        runtime.path(),
        Some(Path::new("rel.sock")),
        &["daemon", "stop"],
    )
    .current_dir(&b)
    .output()
    .expect("run shux");
    assert!(
        String::from_utf8_lossy(&elsewhere.stdout).contains("no daemon running"),
        "daemon stop in another directory claimed it: {}",
        String::from_utf8_lossy(&elsewhere.stdout)
    );
    assert!(
        pid_exists(pid),
        "daemon stop in another directory killed it"
    );

    let here = bin_cmd(
        &bin,
        runtime.path(),
        Some(Path::new("rel.sock")),
        &["daemon", "stop"],
    )
    .current_dir(&a)
    .output()
    .expect("run shux");
    assert!(
        wait_for_pid_gone(pid, Duration::from_secs(5)),
        "daemon stop in its own directory missed it: {}",
        String::from_utf8_lossy(&here.stdout)
    );
}

/// A process that merely carries a daemon's argv is not a daemon: `daemon
/// status` must not report it and `daemon stop` must not signal it.
#[cfg(target_os = "linux")]
#[test]
fn a_process_with_a_daemons_argv_is_not_signalled() {
    let runtime = tempfile::tempdir().expect("temp runtime");
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_shux"));
    let socket = runtime.path().join("shux").join("shux.sock");
    std::fs::write(
        runtime.path().join("__daemon"),
        "import time\ntime.sleep(60)\n",
    )
    .expect("write bystander");
    let mut bystander = Command::new("python3")
        .arg("__daemon")
        .arg("--socket")
        .arg(&socket)
        .current_dir(runtime.path())
        .spawn()
        .expect("spawn bystander");
    let pid = bystander.id();
    let reaper = Reaper(vec![pid as i32]);
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::fs::read(format!("/proc/{pid}/cmdline"))
        .map_or(true, |c| !String::from_utf8_lossy(&c).contains("--socket"))
    {
        assert!(Instant::now() < deadline, "bystander never started");
        thread::sleep(Duration::from_millis(20));
    }

    let status = run_bin(&bin, runtime.path(), None, &["daemon", "status"]);
    let stopped = run_bin(&bin, runtime.path(), None, &["daemon", "stop"]);
    let alive = bystander.try_wait().expect("poll bystander").is_none();
    drop(reaper);
    let _ = bystander.wait();
    assert!(
        String::from_utf8_lossy(&status.stdout).contains("not running"),
        "daemon status reported the bystander: {}",
        String::from_utf8_lossy(&status.stdout)
    );
    assert!(
        alive,
        "daemon stop signalled the bystander: {}",
        String::from_utf8_lossy(&stopped.stdout)
    );
}

/// A daemon from before the socket key named its pidfile after the socket as
/// typed and holds no instance lock, so only that pidfile names it: `daemon
/// stop` given the same relative spelling must still find and stop it.
#[test]
fn a_lockless_daemon_is_stopped_by_the_spelling_it_was_started_with() {
    let runtime = tempfile::tempdir().expect("temp runtime");
    let cwd = tempfile::tempdir().expect("temp cwd");
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_shux"));
    let typed = Path::new("legacy.sock");
    std::fs::write(
        cwd.path().join("__daemon"),
        "import pathlib, time\npathlib.Path('ready').touch()\ntime.sleep(60)\n",
    )
    .expect("write legacy daemon");
    let mut legacy = Command::new("python3")
        .arg("__daemon")
        .arg("--socket")
        .arg(typed)
        .current_dir(cwd.path())
        .spawn()
        .expect("spawn legacy daemon");
    let pid = legacy.id();
    let reaper = Reaper(vec![pid as i32]);
    // Its own ready file, not `/proc`: macOS has none.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !cwd.path().join("ready").exists() {
        assert!(Instant::now() < deadline, "legacy daemon never started");
        thread::sleep(Duration::from_millis(20));
    }
    // FNV-1a of the spelling: the pidfile name earlier builds wrote.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in typed.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let dir = runtime.path().join("shux");
    std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700)
        .create(&dir)
        .expect("runtime dir");
    std::fs::write(dir.join(format!("shux-{hash:016x}.pid")), pid.to_string())
        .expect("legacy pidfile");

    let stopped = bin_cmd(&bin, runtime.path(), Some(typed), &["daemon", "stop"])
        .current_dir(cwd.path())
        .output()
        .expect("run shux");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut exited = false;
    while !exited && Instant::now() < deadline {
        exited = legacy.try_wait().expect("poll legacy daemon").is_some();
        thread::sleep(Duration::from_millis(20));
    }
    drop(reaper);
    let _ = legacy.wait();
    assert!(
        exited,
        "daemon stop missed the legacy daemon: {}{}",
        String::from_utf8_lossy(&stopped.stdout),
        String::from_utf8_lossy(&stopped.stderr)
    );
}

/// The attach socket is rebound when it is unlinked, as the RPC socket is: a
/// daemon from before this build unlinks `attach.sock` on its way out.
#[test]
fn a_daemon_whose_attach_socket_was_unlinked_serves_it_again() {
    let runtime = tempfile::tempdir().expect("temp runtime");
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_shux"));
    let attach = runtime.path().join("shux").join("attach.sock");
    let mut reaper = Reaper(Vec::new());

    assert!(
        run_bin(&bin, runtime.path(), None, &["session", "list"])
            .status
            .success()
    );
    reaper
        .0
        .push(wait_for_pid_file(&runtime.path().join("shux").join("shux.pid")) as i32);
    std::fs::remove_file(&attach).expect("unlink attach socket");
    let deadline = Instant::now() + Duration::from_secs(3);
    while std::os::unix::net::UnixStream::connect(&attach).is_err() {
        assert!(
            Instant::now() < deadline,
            "the attach socket was not rebound"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

/// A client racing the deletion of the whole runtime dir must not leave two
/// daemons behind: the one whose lock file went with the directory either
/// takes a fresh lock again or gives way.
#[test]
fn deleting_the_runtime_dir_strands_no_daemon() {
    let runtime = tempfile::tempdir().expect("temp runtime");
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_shux"));
    let dir = runtime.path().join("shux");
    let socket = dir.join("shux.sock");
    let mut reaper = Reaper(Vec::new());

    for round in 0..3 {
        assert!(
            run_bin(&bin, runtime.path(), None, &["session", "list"])
                .status
                .success(),
            "round {round}: no daemon"
        );
        settle_daemon_count(&socket, 1, &mut reaper);
        std::fs::remove_dir_all(&dir).expect("delete runtime dir");
        let out = run_bin(&bin, runtime.path(), None, &["session", "list"]);
        assert!(
            out.status.success(),
            "round {round}: client after the deletion failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            settle_daemon_count(&socket, 1, &mut reaper),
            1,
            "round {round}: a daemon was stranded"
        );
    }
}

/// One explicit socket used from two runtime dirs is still served by one
/// daemon: neither may unlink a socket the other is already serving.
#[test]
fn one_socket_from_two_runtime_dirs_gets_one_daemon() {
    let shared = tempfile::tempdir().expect("socket dir");
    let (r1, r2) = (
        tempfile::tempdir().expect("runtime 1"),
        tempfile::tempdir().expect("runtime 2"),
    );
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_shux"));
    let socket = shared.path().join("shared.sock");
    let mut reaper = Reaper(Vec::new());

    let clients: Vec<_> = (0..32)
        .map(|i| {
            let runtime = if i % 2 == 0 { r1.path() } else { r2.path() };
            bin_cmd(&bin, runtime, Some(&socket), &["session", "list"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("spawn shux")
        })
        .collect();
    for client in clients {
        let _ = client.wait_with_output().expect("wait for shux");
    }
    assert_eq!(
        settle_daemon_count(&socket, 1, &mut reaper),
        1,
        "one socket from two runtime dirs started more than one daemon"
    );
}

/// A runtime dir replaced by a symlink is never followed by a rebind: the
/// socket must not reappear in whatever directory the link points at.
#[test]
fn a_rebind_does_not_follow_a_runtime_dir_replaced_by_a_symlink() {
    let runtime = tempfile::tempdir().expect("temp runtime");
    let elsewhere = tempfile::tempdir().expect("planted dir");
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_shux"));
    let dir = runtime.path().join("shux");
    let mut reaper = Reaper(Vec::new());

    assert!(
        run_bin(&bin, runtime.path(), None, &["session", "list"])
            .status
            .success()
    );
    reaper
        .0
        .push(wait_for_pid_file(&dir.join("shux.pid")) as i32);
    std::fs::remove_dir_all(&dir).expect("delete runtime dir");
    std::os::unix::fs::symlink(elsewhere.path(), &dir).expect("plant symlink");
    thread::sleep(Duration::from_millis(1500));
    let planted: Vec<_> = std::fs::read_dir(elsewhere.path())
        .expect("read planted dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name())
        .collect();
    assert!(
        planted.is_empty(),
        "the daemon wrote into a symlinked runtime dir: {planted:?}"
    );
}
