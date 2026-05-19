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

fn read_pid(path: &Path) -> u32 {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read pid file {}: {e}", path.display()))
        .trim()
        .parse()
        .unwrap_or_else(|e| panic!("invalid pid in {}: {e}", path.display()))
}

fn wait_for_pid_file(path: &Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if path.exists() {
            let pid = read_pid(path);
            if pid_exists(pid) {
                return pid;
            }
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
