//! One pane-output chunk redraws every attached client (issue #189).
//!
//! Three real attach clients on the real socket of a real daemon. The daemon's
//! fallback tick is pushed far past the test's budget, so a frame that arrives
//! can only have been caused by the pane's output waking that client's render
//! loop. The pane's program prints its marker in one write, when the test
//! releases it through a FIFO, so nothing typed is echoed around it.

use std::path::{Path, PathBuf};
use std::process::Command;

use base64::Engine as _;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use shux_rpc::attach::{ATTACH_PROTOCOL_VERSION, AttachHello, AttachReady, AttachServerFrame};
use shux_testkit::BUDGET;
use shux_vt::{Color, VirtualTerminal};
use tokio::net::UnixStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

const COLS: u16 = 100;
const ROWS: u16 = 30;
/// An hour: no tick lands inside [`BUDGET`].
const NO_TICK_MS: &str = "3600000";

struct Harness {
    bin: PathBuf,
    runtime: tempfile::TempDir,
    work: tempfile::TempDir,
    /// Empty: a config saved meanwhile would wake every render loop.
    config: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        Self {
            bin: PathBuf::from(env!("CARGO_BIN_EXE_shux")),
            runtime: tempfile::tempdir().expect("temp runtime dir"),
            work: tempfile::tempdir().expect("temp work dir"),
            config: tempfile::tempdir().expect("temp config dir"),
        }
    }

    fn runtime_dir(&self) -> &Path {
        self.runtime.path()
    }

    /// The first call starts the daemon, which inherits this environment.
    fn rpc(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let out = Command::new(&self.bin)
            .env("XDG_RUNTIME_DIR", self.runtime_dir())
            .env_remove("SHUX_SOCKET")
            .env("SHELL", "/bin/sh")
            .env("XDG_CONFIG_HOME", self.config.path())
            .env(shux::attach::FALLBACK_TICK_ENV, NO_TICK_MS)
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
        serde_json::from_slice(&out.stdout).expect("rpc json")
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let pid: Option<i32> = std::fs::read_to_string(self.runtime_dir().join("shux/shux.pid"))
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

/// Each word is split in the script, so only the program's output holds it.
const PROBES: &str = "printf '\\033[38;2;120;220;180mTRUE%sCOLOR\\033[0m \\033[38;5;208mINDE%sXED\\033[0m \\033[34mBA%sSIC\\033[0m\\n' '' '' ''";

/// Prints the probes and `READY`, waits for the FIFO, prints `MARK` in one
/// write, then waits on its terminal, where nothing is typed, so nothing
/// follows it and the pane never exits.
fn workload(fifo: &Path) -> String {
    let fifo = fifo.display();
    format!("{PROBES}; printf 'REA%sDY\\n' ''; read x < '{fifo}'; printf 'MA%sRK\\n' ''; read y")
}

/// Releases the pane's program. Non-blocking, so a program that died before
/// opening the FIFO fails the wait instead of hanging the test.
fn release(fifo: &Path) {
    use nix::fcntl::{OFlag, open};
    let fd = shux_testkit::wait_for("the pane's program to open the fifo", || {
        open(
            fifo,
            OFlag::O_WRONLY | OFlag::O_NONBLOCK,
            nix::sys::stat::Mode::empty(),
        )
        .ok()
    });
    nix::unistd::write(&fd, b"go\n").expect("write the fifo");
}

struct Client {
    framed: Framed<UnixStream, LengthDelimitedCodec>,
    vt: VirtualTerminal,
}

impl Client {
    async fn attach(h: &Harness, session: &str) -> Self {
        let sock = h.runtime_dir().join("shux").join("attach.sock");
        let stream = tokio::time::timeout(BUDGET, UnixStream::connect(&sock))
            .await
            .expect("connect timed out")
            .expect("connect attach");
        let mut framed = Framed::new(stream, shux_rpc::create_codec());
        let hello = AttachHello {
            protocol: ATTACH_PROTOCOL_VERSION,
            session_name: Some(session.to_string()),
            cols: COLS,
            rows: ROWS,
            client_version: "multi-client-wake-test".to_string(),
            graphics: false,
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
            AttachReady::Ok { .. } => {}
            AttachReady::Error { code, message } => panic!("attach denied: {code}: {message}"),
        }
        Self {
            framed,
            vt: VirtualTerminal::new(ROWS as usize, COLS as usize),
        }
    }

    /// Replays render frames into the client's screen until it shows `word`,
    /// the daemon hangs up, or [`BUDGET`] runs out.
    async fn draws(&mut self, word: &str) -> bool {
        let deadline = tokio::time::Instant::now() + BUDGET;
        while find_cell(&self.vt, word).is_none() {
            let Ok(Some(Ok(frame))) = tokio::time::timeout_at(deadline, self.framed.next()).await
            else {
                return false;
            };
            if let Ok(AttachServerFrame::Render { data, .. }) = serde_json::from_slice(&frame)
                && let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(&data)
            {
                self.vt.process(&raw);
            }
        }
        true
    }

    fn assert_colour_probes(&self, which: usize) {
        for (probe, want) in [
            ("TRUECOLOR", Color::Rgb(120, 220, 180)),
            ("INDEXED", Color::Indexed(208)),
            ("BASIC", Color::Indexed(4)),
        ] {
            let (row, col) = find_cell(&self.vt, probe)
                .unwrap_or_else(|| panic!("client {which}: {probe} never drew"));
            let fg = self.vt.grid().visible_row(row)[col].style.fg;
            assert_eq!(fg, want, "client {which}: {probe} drew in the wrong colour");
        }
    }
}

/// The renderer positions every cell, so the raw bytes never hold a word
/// contiguously; the replayed screen does.
fn find_cell(vt: &VirtualTerminal, word: &str) -> Option<(usize, usize)> {
    let grid = vt.grid();
    (0..grid.rows()).find_map(|r| {
        let row = grid.visible_row(r);
        let text: String = (0..row.len()).map(|c| row[c].ch).collect();
        text.find(word)
            .map(|byte| (r, text[..byte].chars().count()))
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_output_chunk_redraws_every_attached_client() {
    let h = Harness::new();
    let fifo = h.work.path().join("go");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::from_bits_truncate(0o600)).expect("mkfifo");

    let created = h.rpc(
        "session.create",
        serde_json::json!({
            "name": "wake",
            "command": ["/bin/sh", "-c", workload(&fifo)],
        }),
    );
    let pane = created["result"]["pane_id"]
        .as_str()
        .expect("session.create returns a pane_id")
        .to_string();
    shux_testkit::wait_until("the pane to print READY", || {
        h.rpc("pane.capture", serde_json::json!({ "pane_id": pane }))["result"]["text"]
            .as_str()
            .is_some_and(|t| t.contains("READY"))
    });

    // Each client's first frame comes from the immediate first tick and
    // already holds READY. After that, only a pulse can draw anything.
    let mut clients = Vec::new();
    for which in 0..3 {
        let mut client = Client::attach(&h, "wake").await;
        assert!(
            client.draws("READY").await,
            "client {which} never drew READY"
        );
        client.assert_colour_probes(which);
        clients.push(client);
    }

    release(&fifo);

    let drew = futures::future::join_all(clients.iter_mut().map(|c| c.draws("MARK"))).await;
    let missed: Vec<usize> = (0..drew.len()).filter(|&i| !drew[i]).collect();
    assert!(
        missed.is_empty(),
        "one output chunk redrew only some clients: clients {missed:?} of 3 never drew MARK"
    );
}
