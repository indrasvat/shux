//! A read-only attach on a real daemon sends its panes nothing (issue #191).
//!
//! Two panes of a real session run a recorder that writes every byte its
//! terminal delivers to a file. A writer and a read-only client attach on the
//! real socket; the viewer sends every frame a writer would turn into a write,
//! a focus or layout change or a resize, then leaves. A marker the writer types
//! afterwards must be the first and only thing either pane ever received, and
//! the panes, windows and onboarding state must read as they did before.

use std::path::{Path, PathBuf};
use std::process::Command;

use base64::Engine as _;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use shux_rpc::attach::{
    ATTACH_PROTOCOL_VERSION, ActionArgs, ActionKind, AttachClientFrame, AttachHello, AttachReady,
    AttachServerFrame, MouseButton, MouseKind,
};
use shux_testkit::BUDGET;
use shux_vt::{Color, VirtualTerminal};
use tokio::net::UnixStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

struct Harness {
    bin: PathBuf,
    runtime: tempfile::TempDir,
    config: tempfile::TempDir,
    state: tempfile::TempDir,
    work: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        let state = tempfile::tempdir().expect("temp state dir");
        // The toast already seen, the prefix not yet found: nothing the writer
        // does here changes the file, and any prefix, action or detach that
        // counted would.
        std::fs::create_dir_all(state.path().join("shux")).expect("state dir");
        std::fs::write(
            state.path().join("shux/onboarding.json"),
            r#"{"prefix_discovered": false, "welcome_toast_seen": true}"#,
        )
        .expect("seed onboarding");
        Self {
            bin: PathBuf::from(env!("CARGO_BIN_EXE_shux")),
            runtime: tempfile::tempdir().expect("temp runtime dir"),
            config: tempfile::tempdir().expect("temp config dir"),
            state,
            work: tempfile::tempdir().expect("temp work dir"),
        }
    }

    /// The first call starts the daemon, which inherits this environment.
    fn rpc(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let out = Command::new(&self.bin)
            .env("XDG_RUNTIME_DIR", self.runtime.path())
            .env_remove("SHUX_SOCKET")
            .env("SHELL", "/bin/sh")
            .env("XDG_CONFIG_HOME", self.config.path())
            .env("XDG_STATE_HOME", self.state.path())
            .args([
                "--format",
                "json",
                "rpc",
                "call",
                method,
                "--params",
                &params.to_string(),
            ])
            .output()
            .unwrap_or_else(|e| panic!("shux rpc {method}: {e}"));
        assert!(
            out.status.success(),
            "shux rpc {method} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let mut reply: serde_json::Value = serde_json::from_slice(&out.stdout).expect("rpc json");
        assert!(reply.get("error").is_none(), "shux rpc {method}: {reply}");
        reply["result"].take()
    }

    fn onboarding(&self) -> Option<String> {
        std::fs::read_to_string(self.state.path().join("shux/onboarding.json")).ok()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let pid: Option<i32> = std::fs::read_to_string(self.runtime.path().join("shux/shux.pid"))
            .ok()
            .and_then(|s| s.trim().parse().ok());
        // By pidfile, never by an argv substring.
        if let Some(pid) = pid
            && !shux_testkit::terminate(pid)
            && !std::thread::panicking()
        {
            panic!("daemon {pid} survived SIGTERM");
        }
    }
}

/// Colour probes, then a terminal that hands over every byte as it arrives,
/// asks for the mouse and bracketed paste, and records all it reads.
fn recorder(log: &Path) -> serde_json::Value {
    let script = format!(
        "printf '\\033[38;2;120;220;180mTRUE%sCOLOR\\033[0m \\033[38;5;208mINDE%sXED\\033[0m \\033[34mBA%sSIC\\033[0m\\n' '' '' ''; \
         stty raw -echo; printf '\\033[?1002h\\033[?1006h\\033[?2004h'; printf 'REC%sORDING\\n' ''; \
         exec cat > '{}'",
        log.display()
    );
    serde_json::json!(["sh", "-c", script])
}

struct Client {
    framed: Framed<UnixStream, LengthDelimitedCodec>,
    vt: VirtualTerminal,
    frame: Option<u64>,
}

impl Client {
    async fn attach(h: &Harness, session: &str, cols: u16, rows: u16, read_only: bool) -> Self {
        let sock = h.runtime.path().join("shux").join("attach.sock");
        let stream = tokio::time::timeout(BUDGET, UnixStream::connect(&sock))
            .await
            .expect("connect timed out")
            .expect("connect attach");
        let mut framed = Framed::new(stream, shux_rpc::create_codec());
        let hello = AttachHello {
            protocol: ATTACH_PROTOCOL_VERSION,
            session_name: Some(session.to_string()),
            cols,
            rows,
            client_version: "read-only-test".to_string(),
            graphics: false,
            plain: false,
            read_only,
            no_color: false,
        };
        framed
            .send(Bytes::from(serde_json::to_vec(&hello).expect("hello json")))
            .await
            .expect("send hello");
        let ready = tokio::time::timeout(BUDGET, framed.next())
            .await
            .expect("the daemon never answered the attach hello")
            .expect("ready")
            .expect("ready bytes");
        match serde_json::from_slice::<AttachReady>(&ready).expect("parse ready") {
            AttachReady::Ok {
                read_only: granted,
                protocol,
                ..
            } => {
                assert_eq!(granted, read_only, "the daemon's read-only echo");
                assert_eq!(protocol, ATTACH_PROTOCOL_VERSION);
            }
            AttachReady::Error { code, message } => panic!("attach denied: {code}: {message}"),
        }
        Self {
            framed,
            vt: VirtualTerminal::new(usize::from(rows), usize::from(cols)),
            frame: None,
        }
    }

    async fn send(&mut self, frame: &AttachClientFrame) {
        self.framed
            .send(Bytes::from(serde_json::to_vec(frame).expect("frame json")))
            .await
            .expect("send frame");
    }

    async fn key(&mut self, bytes: &[u8]) {
        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
        self.send(&AttachClientFrame::Input { data }).await;
    }

    /// Replays frames until the screen shows `word`, or [`BUDGET`] runs out.
    async fn draws(&mut self, word: &str) -> bool {
        let deadline = shux_testkit::Deadline::start();
        while find_cell(&self.vt, word).is_none() {
            let Some(Some(Ok(buf))) = deadline.run(self.framed.next()).await else {
                return false;
            };
            if let Ok(AttachServerFrame::Render { data, frame }) = serde_json::from_slice(&buf)
                && let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(&data)
            {
                self.vt.process(&raw);
                if frame.is_some() {
                    self.frame = frame;
                }
            }
        }
        true
    }

    async fn mouse(&mut self, kind: MouseKind, button: MouseButton, (col, row): (u16, u16)) {
        let frame = self.frame;
        self.send(&AttachClientFrame::Mouse {
            kind,
            button,
            col,
            row,
            shift: false,
            alt: false,
            ctrl: false,
            frame,
        })
        .await;
    }
}

fn find_cell(vt: &VirtualTerminal, word: &str) -> Option<(usize, usize)> {
    let grid = vt.grid();
    (0..grid.rows()).find_map(|r| {
        let row = grid.visible_row(r);
        let text: String = (0..row.len()).map(|c| row[c].ch).collect();
        text.find(word)
            .map(|byte| (r, text[..byte].chars().count()))
    })
}

/// What a leaking viewer could change, as the daemon reports it.
fn shared(h: &Harness, session_id: &str) -> serde_json::Value {
    let mut panes = h.rpc("pane.list", serde_json::json!({ "session_id": session_id }));
    let windows = h.rpc(
        "window.list",
        serde_json::json!({ "session_id": session_id }),
    );
    let clients = h.rpc("client.list", serde_json::json!({}));
    let writers = clients["clients"]
        .as_array()
        .expect("clients")
        .iter()
        .filter(|c| c["read_only"] == false)
        .count();
    // Each pane's identity, focus, zoom, version (bumped by any change to it)
    // and its grid size, which only a resize moves.
    for p in panes.as_array_mut().expect("panes") {
        let capture = h.rpc("pane.capture", serde_json::json!({ "pane_id": p["id"] }));
        if let Some(o) = p.as_object_mut() {
            o.retain(|k, _| matches!(k.as_str(), "id" | "is_focused" | "is_zoomed" | "version"));
            o.insert("cols".into(), capture["cols"].clone());
            o.insert("rows".into(), capture["rows"].clone());
        }
    }
    serde_json::json!({ "panes": panes, "windows": windows, "writers": writers })
}

#[tokio::test]
async fn a_real_read_only_attach_sends_its_panes_nothing() {
    let h = Harness::new();
    let (log1, log2) = (h.work.path().join("one.log"), h.work.path().join("two.log"));
    let created = h.rpc(
        "session.create",
        serde_json::json!({ "name": "watched", "command": recorder(&log1) }),
    );
    let session_id = created["id"].as_str().expect("session id").to_string();
    let first = created["pane_id"].as_str().expect("pane id").to_string();
    h.rpc(
        "pane.split",
        serde_json::json!({ "pane_id": first, "direction": "vertical", "command": recorder(&log2) }),
    );
    h.rpc("pane.focus", serde_json::json!({ "pane_id": first }));

    let mut host = Client::attach(&h, "watched", 100, 30, false).await;
    assert!(host.draws("RECORDING").await, "the recorders never started");
    shux_testkit::wait_for("both recorders to open their logs", || {
        (log1.exists() && log2.exists()).then_some(())
    });
    for probe in ["TRUECOLOR", "INDEXED", "BASIC"] {
        assert!(find_cell(&host.vt, probe).is_some(), "{probe} never drew");
    }
    let (row, col) = find_cell(&host.vt, "TRUECOLOR").expect("probe");
    assert_eq!(
        host.vt.grid().visible_row(row)[col].style.fg,
        Color::Rgb(120, 220, 180)
    );
    let before = shared(&h, &session_id);
    let onboarding = h.onboarding();

    let mut viewer = Client::attach(&h, "watched", 160, 48, true).await;
    assert!(
        viewer.draws("viewing · read-only").await,
        "the viewer was never told it is read-only"
    );
    assert!(viewer.draws("RECORDING").await);
    viewer.key(b"typed\r").await;
    viewer
        .send(&AttachClientFrame::Paste {
            data: base64::engine::general_purpose::STANDARD.encode(b"pasted"),
            more: false,
        })
        .await;
    viewer.send(&AttachClientFrame::PrefixTapped).await;
    for kind in [
        ActionKind::SplitSmart,
        ActionKind::FocusNext,
        ActionKind::ToggleZoom,
        ActionKind::KillPane,
        ActionKind::NewWindow,
        ActionKind::NextWindow,
        ActionKind::ResizeLeft,
    ] {
        viewer
            .send(&AttachClientFrame::Action {
                kind,
                args: ActionArgs::default(),
            })
            .await;
    }
    // Both halves of the screen and the border between them.
    for point in [(10, 10), (150, 10), (80, 10)] {
        for kind in [MouseKind::Down, MouseKind::Drag, MouseKind::Up] {
            viewer.mouse(kind, MouseButton::Left, point).await;
        }
        viewer
            .mouse(MouseKind::ScrollUp, MouseButton::None, point)
            .await;
        viewer
            .mouse(MouseKind::Down, MouseButton::Right, point)
            .await;
    }
    viewer
        .send(&AttachClientFrame::Resize { cols: 50, rows: 15 })
        .await;
    viewer.send(&AttachClientFrame::Detach).await;
    let deadline = shux_testkit::Deadline::start();
    loop {
        match deadline.run(viewer.framed.next()).await {
            Some(Some(Ok(buf))) => {
                if matches!(
                    serde_json::from_slice(&buf),
                    Ok(AttachServerFrame::DetachAck)
                ) {
                    break;
                }
            }
            _ => panic!("the viewer's detach was never acknowledged"),
        }
    }

    host.key(b"MARK").await;
    let got = shux_testkit::wait_for("the writer's marker to reach the focused pane", || {
        std::fs::read(&log1).ok().filter(|b| b.ends_with(b"MARK"))
    });
    assert_eq!(
        String::from_utf8_lossy(&got),
        "MARK",
        "the focused pane got more than the writer's marker"
    );
    assert_eq!(
        std::fs::read(&log2).expect("second log"),
        b"",
        "the other pane got input"
    );
    assert_eq!(shared(&h, &session_id), before);
    assert_eq!(
        h.onboarding(),
        onboarding,
        "the viewer wrote onboarding state"
    );
}

/// Strict v2 on the real socket: a v1 hello, read-only or not, is refused
/// before the session it names is created or anything else moves.
#[tokio::test]
async fn an_older_protocol_is_refused_before_anything_moves() {
    let h = Harness::new();
    let log = h.work.path().join("one.log");
    let created = h.rpc(
        "session.create",
        serde_json::json!({ "name": "kept", "command": recorder(&log) }),
    );
    let session_id = created["id"].as_str().expect("session id").to_string();
    let sessions = || h.rpc("session.list", serde_json::json!({}))["sessions"].clone();
    let (before, before_sessions) = (shared(&h, &session_id), sessions());
    for (name, read_only) in [("kept", true), ("kept", false), ("brand-new", true)] {
        let sock = h.runtime.path().join("shux").join("attach.sock");
        let stream = UnixStream::connect(&sock).await.expect("connect attach");
        let mut framed = Framed::new(stream, shux_rpc::create_codec());
        let hello = serde_json::json!({
            "protocol": 1, "session_name": name, "cols": 220, "rows": 70,
            "client_version": "0.59.0", "read_only": read_only,
        });
        framed
            .send(Bytes::from(hello.to_string()))
            .await
            .expect("send hello");
        let reply = tokio::time::timeout(BUDGET, framed.next())
            .await
            .expect("a reply")
            .expect("a frame")
            .expect("bytes");
        match serde_json::from_slice::<AttachReady>(&reply).expect("ready json") {
            AttachReady::Error { code, .. } => assert_eq!(code, "protocol_mismatch"),
            ok => panic!("a v1 hello attached: {ok:?}"),
        }
    }
    assert_eq!(
        sessions(),
        before_sessions,
        "a refused hello created a session"
    );
    assert_eq!(shared(&h, &session_id), before);
    let clients = h.rpc("client.list", serde_json::json!({}));
    assert_eq!(clients["clients"], serde_json::json!([]));
}
