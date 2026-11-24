//! The share gateway (#297) on the daemon's own services, driven over HTTP and
//! WebSocket as a tunnel would deliver them: plaintext, from loopback, with
//! the public origin's `Host`. The browser rig (`make test-share-browser`)
//! drives the same gateway through TLS in a real browser.
//!
//! Panes run real programs on real PTYs. A recorder pane writes every byte
//! its terminal delivers to a file, so "the viewer sent nothing" is read
//! from the pane's side, after a marker a writer typed.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use base64::Engine as _;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use shux::daemon_boot::InProcess;
use shux::share::{CreateOpts, Gateway, GrantId, Origin, Uses};
use shux_rpc::attach::{
    ATTACH_PROTOCOL_VERSION, AttachClientFrame, AttachHello, AttachReady, AttachServerFrame,
};
use shux_testkit::BUDGET;
use shux_vt::{Color, VirtualTerminal};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UnixStream};
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

const HOST: &str = "share.test";
const ORIGIN: &str = "https://share.test";

struct Dirs {
    runtime: tempfile::TempDir,
    _config: tempfile::TempDir,
    _state: tempfile::TempDir,
    work: tempfile::TempDir,
}

/// One set of directories for the process, set before any runtime starts.
fn dirs() -> &'static Dirs {
    static DIRS: OnceLock<Dirs> = OnceLock::new();
    DIRS.get_or_init(|| {
        let dirs = Dirs {
            runtime: tempfile::tempdir().expect("runtime dir"),
            _config: tempfile::tempdir().expect("config dir"),
            _state: tempfile::tempdir().expect("state dir"),
            work: tempfile::tempdir().expect("work dir"),
        };
        std::fs::create_dir_all(dirs._state.path().join("shux")).expect("state");
        std::fs::write(
            dirs._state.path().join("shux/onboarding.json"),
            r#"{"prefix_discovered": true, "welcome_toast_seen": true}"#,
        )
        .expect("seed onboarding");
        // SAFETY: every test calls this first, before it starts a runtime;
        // any other test thread is blocked on this `OnceLock` meanwhile, and
        // nothing else in the process reads the environment concurrently.
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", dirs.runtime.path());
            std::env::set_var("XDG_CONFIG_HOME", dirs._config.path());
            std::env::set_var("XDG_STATE_HOME", dirs._state.path());
            std::env::set_var("SHELL", "/bin/sh");
            std::env::remove_var("SHUX_SOCKET");
        }
        dirs
    })
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

struct Rig {
    daemon: Option<InProcess>,
    gw: Gateway,
    addr: SocketAddr,
    socket: PathBuf,
}

impl Rig {
    async fn start(name: &str) -> Self {
        let socket = dirs().runtime.path().join(format!("{name}.sock"));
        let daemon = InProcess::start(socket.clone())
            .await
            .expect("daemon services");
        let gw = daemon.gateway.clone();
        let attach = shux::daemon::attach_socket_path_for(&socket).expect("attach path");
        for path in [&socket, &attach] {
            let p = path.clone();
            shux_testkit::wait_until_async("the daemon's sockets", || {
                let p = p.clone();
                async move { UnixStream::connect(&p).await.is_ok() }
            })
            .await;
        }
        let addr = gw
            .listen("127.0.0.1:0".parse().unwrap())
            .await
            .expect("listen");
        gw.set_origin(Origin::parse(ORIGIN).unwrap())
            .await
            .expect("origin");
        Self {
            daemon: Some(daemon),
            gw,
            addr,
            socket,
        }
    }

    async fn stop(mut self) {
        if let Some(d) = self.daemon.take() {
            d.shutdown().await;
        }
    }

    async fn rpc(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_shux"))
            .env("SHUX_SOCKET", &self.socket)
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
            .await
            .unwrap_or_else(|e| panic!("shux rpc {method}: {e}"));
        assert!(
            out.status.success(),
            "shux rpc {method}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let mut reply: serde_json::Value = serde_json::from_slice(&out.stdout).expect("rpc json");
        assert!(reply.get("error").is_none(), "shux rpc {method}: {reply}");
        reply["result"].take()
    }

    /// A session running `command` in its one pane: (session id, pane id).
    async fn session(&self, name: &str, command: serde_json::Value) -> (String, String) {
        let created = self
            .rpc(
                "session.create",
                serde_json::json!({ "name": name, "command": command }),
            )
            .await;
        (
            created["id"].as_str().expect("id").into(),
            created["pane_id"].as_str().expect("pane").into(),
        )
    }

    fn link(&self, session: &str, opts: impl FnOnce(&mut CreateOpts)) -> (GrantId, String) {
        let mut o = CreateOpts::new(session.parse().expect("session id"));
        opts(&mut o);
        let created = self.gw.create(o).expect("create a link");
        let secret = created.url.split_once('#').expect("fragment").1.to_string();
        assert_eq!(
            created.url,
            format!("{ORIGIN}/v/{}#{secret}", created.id.as_str()),
            "the link's shape"
        );
        (created.id, secret)
    }

    async fn redeem(&self, id: &GrantId, secret: &str) -> Reply {
        let body = serde_json::json!({ "secret": secret }).to_string();
        http(
            self.addr,
            "POST",
            &format!("/v/{}/redeem", id.as_str()),
            &[
                ("Host", HOST),
                ("Origin", ORIGIN),
                ("Content-Type", "application/json"),
            ],
            &body,
        )
        .await
    }

    /// A viewer, redeemed and connected.
    async fn viewer(&self, id: &GrantId, secret: &str, cols: u16, rows: u16) -> Viewer {
        let reply = self.redeem(id, secret).await;
        assert_eq!(reply.status, 200, "redeem: {}", reply.body);
        let cookie = reply.cookie(id);
        Viewer::connect(self.addr, id, &cookie, ORIGIN, cols, rows)
            .await
            .expect("the viewer connects")
    }
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    /// `name=value` of this link's cookie, from `Set-Cookie`.
    fn cookie(&self, id: &GrantId) -> String {
        let set = self.header("set-cookie").expect("a cookie");
        let pair = set.split(';').next().expect("pair");
        assert!(pair.starts_with(&format!("{}=", id.cookie_name())), "{set}");
        pair.to_string()
    }
}

async fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Reply {
    let mut tcp = TcpStream::connect(addr).await.expect("connect");
    let mut req = format!("{method} {path} HTTP/1.1\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    tcp.write_all(req.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    tokio::time::timeout(BUDGET, tcp.read_to_end(&mut raw))
        .await
        .expect("the gateway never answered")
        .expect("read");
    let text = String::from_utf8_lossy(&raw).to_string();
    let (head, body) = text.split_once("\r\n\r\n").expect("a response");
    let mut lines = head.lines();
    let status = lines.next().expect("status")[9..12].parse().expect("code");
    let headers = lines
        .filter_map(|l| l.split_once(": "))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Reply {
        status,
        headers,
        body: body.to_string(),
    }
}

type Ws = tokio_tungstenite::WebSocketStream<TcpStream>;

/// A browser's side of `shux.view.v1`, with a terminal of its own.
struct Viewer {
    ws: Ws,
    vt: VirtualTerminal,
    received: u64,
    ready: serde_json::Value,
    /// Acknowledge what it parses: a stalled renderer does not.
    acks: bool,
    ended: Option<String>,
    resizes: Vec<(u16, u16)>,
    closed: bool,
}

async fn upgrade(addr: SocketAddr, id: &GrantId, cookie: &str, origin: &str) -> Result<Ws, u16> {
    let mut req = format!("ws://{HOST}/v/{}/ws", id.as_str())
        .into_client_request()
        .expect("request");
    req.headers_mut().insert("Origin", origin.parse().unwrap());
    req.headers_mut().insert("Cookie", cookie.parse().unwrap());
    let tcp = TcpStream::connect(addr).await.expect("connect");
    match tokio_tungstenite::client_async(req, tcp).await {
        Ok((ws, _)) => Ok(ws),
        Err(tungstenite::Error::Http(res)) => Err(res.status().as_u16()),
        Err(e) => panic!("upgrade: {e}"),
    }
}

impl Viewer {
    async fn connect(
        addr: SocketAddr,
        id: &GrantId,
        cookie: &str,
        origin: &str,
        cols: u16,
        rows: u16,
    ) -> Result<Self, u16> {
        let mut ws = upgrade(addr, id, cookie, origin).await?;
        let hello = serde_json::json!({ "t": "hello", "cols": cols, "rows": rows });
        ws.send(tungstenite::Message::text(hello.to_string()))
            .await
            .expect("hello");
        let mut v = Self {
            ws,
            vt: VirtualTerminal::new(usize::from(rows), usize::from(cols)),
            received: 0,
            ready: serde_json::Value::Null,
            acks: true,
            ended: None,
            resizes: Vec::new(),
            closed: false,
        };
        while v.ready.is_null() {
            assert!(
                v.step().await,
                "the stream closed before ready: {:?}",
                v.ended
            );
        }
        Ok(v)
    }

    /// Handles one message; false once the stream has closed.
    async fn step(&mut self) -> bool {
        let Some(Some(message)) = shux_testkit::Deadline::start().run(self.ws.next()).await else {
            panic!("the stream went quiet");
        };
        match message {
            Ok(tungstenite::Message::Text(t)) => {
                let m: serde_json::Value = serde_json::from_str(&t).expect("view json");
                match m["t"].as_str() {
                    Some("ready") => {
                        let (c, r) = (
                            m["size"]["cols"].as_u64().unwrap(),
                            m["size"]["rows"].as_u64().unwrap(),
                        );
                        self.vt = VirtualTerminal::new(r as usize, c as usize);
                        self.ready = m;
                    }
                    Some("resize") => {
                        let (c, r) = (
                            m["cols"].as_u64().unwrap() as u16,
                            m["rows"].as_u64().unwrap() as u16,
                        );
                        assert_eq!(
                            m["at"].as_u64(),
                            Some(self.received),
                            "the boundary's place"
                        );
                        self.vt.resize(usize::from(r), usize::from(c));
                        self.resizes.push((c, r));
                    }
                    Some("ended") => self.ended = m["reason"].as_str().map(str::to_string),
                    _ => {}
                }
                true
            }
            Ok(tungstenite::Message::Binary(b)) => {
                let offset = u64::from_be_bytes(b[..8].try_into().unwrap());
                assert_eq!(offset, self.received, "a chunk out of order");
                assert!(b.len() - 8 <= shux::share::wire::CHUNK);
                self.vt.process(&b[8..]);
                self.received += (b.len() - 8) as u64;
                if self.acks {
                    let ack = serde_json::json!({ "t": "ack", "offset": self.received });
                    let _ = self
                        .ws
                        .send(tungstenite::Message::text(ack.to_string()))
                        .await;
                }
                true
            }
            Ok(tungstenite::Message::Close(_)) | Err(_) => {
                self.closed = true;
                false
            }
            Ok(_) => true,
        }
    }

    async fn draws(&mut self, word: &str) -> bool {
        while find(&self.vt, word).is_none() {
            if !self.step().await {
                return false;
            }
        }
        true
    }

    /// Reads to the close; the reason the stream gave.
    async fn ends(&mut self) -> Option<String> {
        while !self.closed && self.step().await {}
        self.ended.clone()
    }

    async fn send(&mut self, m: serde_json::Value) {
        self.ws
            .send(tungstenite::Message::text(m.to_string()))
            .await
            .expect("send");
    }
}

fn rows_of(vt: &VirtualTerminal) -> Vec<String> {
    let grid = vt.grid();
    (0..grid.rows())
        .map(|r| {
            let row = grid.visible_row(r);
            (0..row.len()).map(|c| row[c].ch).collect()
        })
        .collect()
}

fn find(vt: &VirtualTerminal, word: &str) -> Option<(usize, usize)> {
    rows_of(vt)
        .iter()
        .enumerate()
        .find_map(|(r, text)| text.find(word).map(|b| (r, text[..b].chars().count())))
}

/// Colour probes, then a raw terminal that records every byte it is given.
fn recorder(log: &Path) -> serde_json::Value {
    let script = format!(
        "printf '\\033[38;2;120;220;180mTRUE%sCOLOR\\033[0m \\033[38;5;208mINDE%sXED\\033[0m \\033[34mBA%sSIC\\033[0m\\n' '' '' ''; \
         stty raw -echo; printf 'REC%sORDING\\n' ''; exec cat > '{}'",
        log.display()
    );
    serde_json::json!(["sh", "-c", script])
}

/// Output without end, numbered.
fn producer() -> serde_json::Value {
    serde_json::json!([
        "sh",
        "-c",
        "i=0; while :; do i=$((i+1)); echo \"TICK $i\"; done"
    ])
}

/// A writable client, drained in the background so it never stalls: its
/// screen's text and how many frames it has drawn.
struct Host {
    keys: tokio::sync::mpsc::Sender<Vec<u8>>,
    seen: tokio::sync::watch::Receiver<(u64, String)>,
    task: tokio::task::JoinHandle<()>,
}

impl Host {
    async fn attach(rig: &Rig, session: &str, cols: u16, rows: u16) -> Self {
        let mut client = HostClient::attach(rig, session, cols, rows).await;
        let (keys, mut key_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
        let (tx, seen) = tokio::sync::watch::channel((0, String::new()));
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    key = key_rx.recv() => match key {
                        Some(k) => client.key(&k).await,
                        None => break,
                    },
                    alive = client.step() => {
                        if !alive {
                            break;
                        }
                        tx.send_replace((client.frames, rows_of(&client.vt).join("\n")));
                    }
                }
            }
        });
        Self { keys, seen, task }
    }

    async fn draws(&mut self, word: &str) {
        let found = self.seen.wait_for(|(_, text)| text.contains(word));
        assert!(
            tokio::time::timeout(BUDGET, found)
                .await
                .is_ok_and(|r| r.is_ok()),
            "the host never drew {word}"
        );
    }

    fn frames(&self) -> u64 {
        self.seen.borrow().0
    }

    async fn frames_past(&mut self, n: u64) {
        let past = self.seen.wait_for(|(f, _)| *f > n);
        assert!(
            tokio::time::timeout(BUDGET, past)
                .await
                .is_ok_and(|r| r.is_ok()),
            "the host stopped drawing"
        );
    }

    async fn key(&self, bytes: &[u8]) {
        self.keys
            .send(bytes.to_vec())
            .await
            .expect("the host is attached");
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct HostClient {
    framed: Framed<UnixStream, LengthDelimitedCodec>,
    vt: VirtualTerminal,
    frames: u64,
}

impl HostClient {
    async fn attach(rig: &Rig, session: &str, cols: u16, rows: u16) -> Self {
        let path = shux::daemon::attach_socket_path_for(&rig.socket).unwrap();
        let stream = UnixStream::connect(path).await.expect("attach socket");
        let mut framed = Framed::new(stream, shux_rpc::create_codec());
        let hello = AttachHello {
            protocol: ATTACH_PROTOCOL_VERSION,
            session_name: Some(session.into()),
            cols,
            rows,
            client_version: "share-test-host".into(),
            graphics: false,
            plain: false,
            read_only: false,
            no_color: false,
        };
        framed
            .send(Bytes::from(serde_json::to_vec(&hello).unwrap()))
            .await
            .unwrap();
        let ready = framed.next().await.expect("ready").expect("ready");
        assert!(matches!(
            serde_json::from_slice(&ready).unwrap(),
            AttachReady::Ok {
                read_only: false,
                ..
            }
        ));
        Self {
            framed,
            vt: VirtualTerminal::new(usize::from(rows), usize::from(cols)),
            frames: 0,
        }
    }

    /// One frame; false once the stream has ended.
    async fn step(&mut self) -> bool {
        let Some(Ok(buf)) = self.framed.next().await else {
            return false;
        };
        match serde_json::from_slice(&buf).expect("frame") {
            AttachServerFrame::Render { data, .. } => {
                self.vt.process(
                    &base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .unwrap(),
                );
                self.frames += 1;
            }
            AttachServerFrame::Ping => {
                let pong = serde_json::to_vec(&AttachClientFrame::Pong).unwrap();
                let _ = self.framed.send(Bytes::from(pong)).await;
            }
            _ => {}
        }
        true
    }

    async fn key(&mut self, bytes: &[u8]) {
        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
        let input = serde_json::to_vec(&AttachClientFrame::Input { data }).unwrap();
        self.framed.send(Bytes::from(input)).await.unwrap();
    }
}

async fn pane_size(rig: &Rig, pane: &str) -> (u64, u64) {
    let c = rig
        .rpc("pane.capture", serde_json::json!({ "pane_id": pane }))
        .await;
    (c["cols"].as_u64().unwrap(), c["rows"].as_u64().unwrap())
}

#[test]
fn a_browser_view_sends_nothing_never_sizes_the_host_and_draws_no_status_bar() {
    let d = dirs();
    runtime().block_on(async {
        let rig = Rig::start("readonly").await;
        let log = d.work.path().join("readonly.log");
        let (session, pane) = rig.session("watched", recorder(&log)).await;
        let mut host = Host::attach(&rig, "watched", 100, 30).await;
        host.draws("RECORDING").await;
        let opened = log.clone();
        shux_testkit::wait_until_async("the recorder opens its log", || {
            let log = opened.clone();
            async move { log.exists() }
        })
        .await;
        let size = pane_size(&rig, &pane).await;

        let (id, secret) = rig.link(&session, |_| {});
        let mut viewer = rig.viewer(&id, &secret, 160, 48).await;
        assert_eq!(viewer.ready["v"], "shux.view.v1");
        assert_eq!(viewer.ready["session"], "watched");
        assert_eq!(
            viewer.ready["host"],
            serde_json::json!({ "cols": 100, "rows": 29 })
        );
        assert!(
            viewer.draws("RECORDING").await,
            "the viewer never saw the pane"
        );
        for probe in ["TRUECOLOR", "INDEXED", "BASIC"] {
            assert!(find(&viewer.vt, probe).is_some(), "{probe} never drew");
        }
        let (r, c) = find(&viewer.vt, "TRUECOLOR").unwrap();
        assert_eq!(
            viewer.vt.grid().visible_row(r)[c].style.fg,
            Color::Rgb(120, 220, 180)
        );
        // The status bar names the session and the viewer's mode; the panes
        // name neither.
        let rows = rows_of(&viewer.vt);
        assert!(
            !rows
                .iter()
                .any(|r| r.contains("watched") || r.contains("read-only")),
            "a native status segment reached the browser:\n{}",
            rows.join("\n")
        );

        // Resizes reach this viewer's own grid, never past the host's layout.
        for (ask, got) in [((50, 15), (50, 15)), ((300, 300), (100, 29))] {
            viewer
                .send(serde_json::json!({ "t": "resize", "cols": ask.0, "rows": ask.1 }))
                .await;
            while viewer.resizes.last() != Some(&got) {
                assert!(viewer.step().await);
            }
        }
        assert!(
            viewer.draws("RECORDING").await,
            "nothing drawn at the new size"
        );
        // The registry holds it as a watcher: it can never drive the panes,
        // even once the host has gone.
        let listed = rig.rpc("client.list", serde_json::json!({})).await;
        let watchers = listed["clients"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["read_only"] == true)
            .count();
        assert_eq!(watchers, 1, "{listed}");
        // A viewer that tries to type is cut off; its bytes go nowhere.
        viewer
            .send(serde_json::json!({ "t": "input", "data": "rm -rf ~\r" }))
            .await;
        assert_eq!(viewer.ends().await.as_deref(), Some("protocol"));

        let listed = rig.rpc("client.list", serde_json::json!({})).await;
        let clients = listed["clients"].as_array().unwrap();
        assert_eq!(
            clients.len(),
            1,
            "the viewer's attach outlived it: {listed}"
        );
        assert_eq!(
            pane_size(&rig, &pane).await,
            size,
            "the viewer sized the host's pane"
        );
        host.key(b"MARK").await;
        let written = log.clone();
        shux_testkit::wait_until_async("the host's key reaches the pane", || {
            let log = written.clone();
            async move { std::fs::read(&log).is_ok_and(|b| b.len() >= 4) }
        })
        .await;
        assert_eq!(
            std::fs::read(&log).unwrap(),
            b"MARK",
            "the pane received more than the host's key"
        );
        rig.stop().await;
    });
}

#[test]
fn a_one_use_link_redeems_exactly_once_under_a_race() {
    let _ = dirs();
    runtime().block_on(async {
        let rig = Rig::start("race").await;
        let (session, _) = rig
            .session("raced", serde_json::json!(["sh", "-c", "exec cat"]))
            .await;
        let (id, secret) = rig.link(&session, |_| {});
        let racers = (0..16).map(|_| rig.redeem(&id, &secret));
        let replies = futures::future::join_all(racers).await;
        let ok = replies.iter().filter(|r| r.status == 200).count();
        let gone = replies.iter().filter(|r| r.status == 410).count();
        assert_eq!(
            (ok, gone),
            (1, 15),
            "{:?}",
            replies.iter().map(|r| r.status).collect::<Vec<_>>()
        );
        let listed = rig.gw.list();
        assert_eq!((listed[0].uses_left, listed[0].redeemed), (Some(0), 1));
        rig.stop().await;
    });
}

#[test]
fn every_route_checks_host_origin_and_shape_and_hardens_its_reply() {
    let _ = dirs();
    runtime().block_on(async {
        let rig = Rig::start("checks").await;
        let (session, _) = rig
            .session("checked", serde_json::json!(["sh", "-c", "exec cat"]))
            .await;
        let (id, secret) = rig.link(&session, |o| o.uses = Uses::Unlimited);
        let path = format!("/v/{}", id.as_str());
        let page = http(rig.addr, "GET", &path, &[("Host", HOST)], "").await;
        assert_eq!(page.status, 200);
        assert!(!page.body.contains(&secret) && !page.body.contains("og:"));
        for (name, want) in [
            ("referrer-policy", "no-referrer"),
            ("cache-control", "no-store"),
            ("x-content-type-options", "nosniff"),
            ("x-robots-tag", "noindex, nofollow"),
        ] {
            assert_eq!(page.header(name), Some(want), "{name}");
        }
        let csp = page.header("content-security-policy").expect("csp");
        assert!(csp.contains("frame-ancestors 'none'") && csp.contains("default-src 'none'"));
        assert!(
            !csp.contains("unsafe-inline") && !csp.contains("http"),
            "{csp}"
        );
        let wasm = http(rig.addr, "GET", "/_/ghostty-vt.wasm", &[("Host", HOST)], "").await;
        assert_eq!(
            (wasm.status, wasm.header("content-type")),
            (200, Some("application/wasm"))
        );
        // Another host, even the loopback one the tunnel dials: not served.
        for host in ["evil.test", "127.0.0.1", "share.test.evil.test"] {
            let r = http(rig.addr, "GET", &path, &[("Host", host)], "").await;
            assert_eq!(r.status, 404, "{host}");
        }
        let body = serde_json::json!({ "secret": secret }).to_string();
        let redeem = format!("{path}/redeem");
        for (headers, want) in [
            (
                vec![("Host", HOST), ("Content-Type", "application/json")],
                403,
            ),
            (
                vec![
                    ("Host", HOST),
                    ("Origin", "https://evil.test"),
                    ("Content-Type", "application/json"),
                ],
                403,
            ),
            (
                vec![
                    ("Host", HOST),
                    ("Origin", "null"),
                    ("Content-Type", "application/json"),
                ],
                403,
            ),
            (
                vec![
                    ("Host", HOST),
                    ("Origin", ORIGIN),
                    ("Content-Type", "text/plain"),
                ],
                415,
            ),
            (
                vec![
                    ("Host", "evil.test"),
                    ("Origin", ORIGIN),
                    ("Content-Type", "application/json"),
                ],
                404,
            ),
        ] {
            assert_eq!(
                http(rig.addr, "POST", &redeem, &headers, &body)
                    .await
                    .status,
                want,
                "{headers:?}"
            );
        }
        assert_eq!(
            rig.gw.list()[0].redeemed,
            0,
            "a refused request spent a use"
        );
        let wrong = serde_json::json!({ "secret": "A".repeat(43) }).to_string();
        let r = http(
            rig.addr,
            "POST",
            &redeem,
            &[
                ("Host", HOST),
                ("Origin", ORIGIN),
                ("Content-Type", "application/json"),
            ],
            &wrong,
        )
        .await;
        assert_eq!(r.status, 404, "a wrong secret reads as no link");
        let r = rig.redeem(&id, &secret).await;
        let set = r.header("set-cookie").unwrap();
        for attr in [
            "Path=/",
            "Secure",
            "HttpOnly",
            "SameSite=Strict",
            "Max-Age=",
        ] {
            assert!(set.contains(attr), "{attr} missing from {set}");
        }
        assert!(set.starts_with(&format!("__Host-shux-v-{}=", id.as_str())));
        let cookie = r.cookie(&id);
        // The socket wants the exact origin and this link's cookie.
        assert_eq!(
            upgrade(rig.addr, &id, &cookie, "https://evil.test")
                .await
                .err(),
            Some(403)
        );
        let (other, other_secret) = rig.link(&session, |_| {});
        let other_cookie = rig.redeem(&other, &other_secret).await.cookie(&other);
        assert_eq!(
            upgrade(rig.addr, &id, &other_cookie, ORIGIN).await.err(),
            Some(403)
        );
        // Two links on one origin stream side by side.
        let a = Viewer::connect(rig.addr, &id, &cookie, ORIGIN, 80, 24)
            .await
            .expect("a");
        let b = Viewer::connect(rig.addr, &other, &other_cookie, ORIGIN, 80, 24)
            .await
            .expect("b");
        let status = http(
            rig.addr,
            "GET",
            &format!("{path}/status"),
            &[("Host", HOST), ("Cookie", &cookie)],
            "",
        )
        .await;
        assert_eq!(status.status, 200);
        assert_eq!(
            rig.gw
                .list()
                .iter()
                .map(|g| g.viewers.iter().filter(|v| v.connected).count())
                .sum::<usize>(),
            2
        );
        drop((a, b));
        rig.stop().await;
    });
}

#[test]
fn a_reconnect_replaces_its_stream_with_a_full_redraw() {
    let _ = dirs();
    runtime().block_on(async {
        let rig = Rig::start("refresh").await;
        let (session, _) = rig
            .session(
                "refreshed",
                serde_json::json!(["sh", "-c", "printf 'HEL%sLO\\n' ''; exec cat"]),
            )
            .await;
        let (id, secret) = rig.link(&session, |_| {});
        let r = rig.redeem(&id, &secret).await;
        let cookie = r.cookie(&id);
        let mut first = Viewer::connect(rig.addr, &id, &cookie, ORIGIN, 80, 24)
            .await
            .unwrap();
        assert!(first.draws("HELLO").await);
        // A reload: the same cookie, a fresh stream drawn from nothing.
        let mut second = Viewer::connect(rig.addr, &id, &cookie, ORIGIN, 80, 24)
            .await
            .unwrap();
        assert_eq!(first.ends().await.as_deref(), Some("replaced"));
        assert!(second.draws("HELLO").await, "the new stream did not redraw");
        let listed = rig.gw.list();
        assert_eq!(listed[0].viewers.len(), 1, "a reload is not a new viewer");
        assert_eq!(listed[0].redeemed, 1);
        rig.stop().await;
    });
}

#[test]
fn revoke_ends_the_live_stream_at_the_origin_and_its_reconnect() {
    let _ = dirs();
    runtime().block_on(async {
        let rig = Rig::start("revoke").await;
        let (session, _) = rig.session("revoked", producer()).await;
        let mut host = Host::attach(&rig, "revoked", 80, 24).await;
        host.draws("TICK").await;
        let (id, secret) = rig.link(&session, |o| o.uses = Uses::Unlimited);
        let mut live = rig.viewer(&id, &secret, 80, 24).await;
        assert!(live.draws("TICK").await);
        let r = rig.redeem(&id, &secret).await;
        let offline = r.cookie(&id);

        rig.gw.revoke(&id).await.expect("revoke");
        // Once revoke returns the origin writes nothing more, though the pane
        // keeps printing and the host keeps drawing it.
        let sent = rig.gw.bytes_sent();
        let seen = host.frames();
        host.frames_past(seen + 20).await;
        assert_eq!(
            rig.gw.bytes_sent(),
            sent,
            "the origin kept sending after revoke"
        );
        assert_eq!(live.ends().await.as_deref(), Some("revoked"));
        // The offline credential does not get back in.
        assert_eq!(
            upgrade(rig.addr, &id, &offline, ORIGIN).await.err(),
            Some(403)
        );
        let status = http(
            rig.addr,
            "GET",
            &format!("/v/{}/status", id.as_str()),
            &[("Host", HOST), ("Cookie", &offline)],
            "",
        )
        .await;
        assert_eq!(status.status, 410);
        assert_eq!(rig.redeem(&id, &secret).await.status, 404);
        assert!(rig.gw.list().is_empty());
        rig.stop().await;
    });
}

#[test]
fn deleting_the_session_ends_its_viewers_and_links() {
    let _ = dirs();
    runtime().block_on(async {
        let rig = Rig::start("delete").await;
        let (session, _) = rig.session("doomed", producer()).await;
        let (id, secret) = rig.link(&session, |o| o.uses = Uses::Unlimited);
        let mut viewer = rig.viewer(&id, &secret, 80, 24).await;
        assert!(viewer.draws("TICK").await);
        let offline = rig.redeem(&id, &secret).await.cookie(&id);
        rig.rpc("session.kill", serde_json::json!({ "name": "doomed" }))
            .await;
        assert_eq!(viewer.ends().await.as_deref(), Some("session_ended"));
        assert_eq!(
            upgrade(rig.addr, &id, &offline, ORIGIN).await.err(),
            Some(403)
        );
        assert!(rig.gw.list().is_empty(), "a link outlived its session");
        rig.stop().await;
    });
}

#[test]
fn a_stalled_renderer_never_holds_more_than_the_window_and_is_dropped() {
    let _ = dirs();
    runtime().block_on(async {
        let rig = Rig::start("stall").await;
        // A wide pane full of coloured scrolling lines: far more than the
        // window within the ack deadline.
        let flood = serde_json::json!([
            "sh",
            "-c",
            "i=0; while :; do i=$((i+1)); printf \"\\033[3%dmTICK %s %0200d\\033[0m\\n\" $((i%7+1)) $i 0; done"
        ]);
        let (session, _) = rig.session("flooded", flood).await;
        let mut host = Host::attach(&rig, "flooded", 240, 101).await;
        host.draws("TICK").await;
        let (id, secret) = rig.link(&session, |_| {});
        let r = rig.redeem(&id, &secret).await;
        let cookie = r.cookie(&id);
        let mut stalled = Viewer::connect(rig.addr, &id, &cookie, ORIGIN, 240, 100)
            .await
            .unwrap();
        stalled.acks = false;
        assert_eq!(stalled.ends().await.as_deref(), Some("lagging"));
        let window = shux::share::wire::WINDOW;
        assert!(
            stalled.received <= window,
            "{} bytes reached a viewer that acknowledged none",
            stalled.received
        );
        // Not vacuous: the stream filled the window before it was dropped.
        assert!(
            stalled.received > window - shux::share::wire::CHUNK as u64,
            "only {} bytes were sent; the window was never reached",
            stalled.received
        );
        // It comes back to a whole screen.
        let mut back = Viewer::connect(rig.addr, &id, &cookie, ORIGIN, 80, 24)
            .await
            .unwrap();
        assert!(back.draws("TICK").await);
        rig.stop().await;
    });
}

#[test]
fn kick_stop_and_expiry_end_live_viewers() {
    let d = dirs();
    // One thread, so the test can drive the gateway's clock.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let rig = Rig::start("ends").await;
        let log = d.work.path().join("ends.log");
        let (session, _) = rig.session("ended", recorder(&log)).await;
        let (id, secret) = rig.link(&session, |o| o.uses = Uses::Unlimited);
        let mut kicked = rig.viewer(&id, &secret, 80, 24).await;
        assert!(kicked.draws("RECORDING").await);
        let viewer = rig.gw.list()[0].viewers[0].id.clone();
        rig.gw.kick(&viewer).await.expect("kick");
        assert_eq!(kicked.ends().await.as_deref(), Some("kicked"));
        assert_eq!(rig.gw.list()[0].viewers.len(), 0);

        // A live viewer, then its link's lifetime passes on the gateway's
        // clock: no race with real time.
        let ttl = shux::share::grants::DEFAULT_TTL / 2;
        let (short, short_secret) = rig.link(&session, |o| o.ttl = ttl);
        let cookie = rig.redeem(&short, &short_secret).await.cookie(&short);
        let mut expiring = Viewer::connect(rig.addr, &short, &cookie, ORIGIN, 80, 24)
            .await
            .unwrap();
        assert!(expiring.draws("RECORDING").await);
        tokio::time::pause();
        tokio::time::advance(ttl).await;
        tokio::time::resume();
        assert_eq!(expiring.ends().await.as_deref(), Some("expired"));
        assert_eq!(
            upgrade(rig.addr, &short, &cookie, ORIGIN).await.err(),
            Some(403)
        );

        let mut stopped = rig.viewer(&id, &secret, 80, 24).await;
        assert!(stopped.draws("RECORDING").await);
        rig.gw.stop().await.expect("stop");
        assert_eq!(stopped.ends().await.as_deref(), Some("stopped"));
        assert!(rig.gw.list().is_empty());
        assert!(
            TcpStream::connect(rig.addr).await.is_err(),
            "the listener outlived stop"
        );
        rig.gw.stop().await.expect("stop");
        rig.stop().await;
    });
}

/// One client frame, masked with a zero key: legal, and the payload stays as is.
fn frame(op: u8, payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() < 126, "a short frame");
    let mut f = vec![0x80 | op, 0x80 | payload.len() as u8, 0, 0, 0, 0];
    f.extend_from_slice(payload);
    f
}

/// A WebSocket upgrade by hand, so a test can write frames no browser would.
async fn raw_upgrade(addr: SocketAddr, id: &GrantId, cookie: &str) -> TcpStream {
    raw_upgrade_on(TcpStream::connect(addr).await.expect("connect"), id, cookie).await
}

async fn raw_upgrade_on(mut tcp: TcpStream, id: &GrantId, cookie: &str) -> TcpStream {
    let req = format!(
        "GET /v/{}/ws HTTP/1.1\r\nHost: {HOST}\r\nOrigin: {ORIGIN}\r\nCookie: {cookie}\r\n\
         Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
        id.as_str()
    );
    tcp.write_all(req.as_bytes())
        .await
        .expect("upgrade request");
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut b = [0u8; 1];
        tcp.read_exact(&mut b).await.expect("the upgrade reply");
        head.push(b[0]);
    }
    assert!(
        head.starts_with(b"HTTP/1.1 101"),
        "{}",
        String::from_utf8_lossy(&head)
    );
    tcp
}

#[test]
fn an_excessive_ping_sender_is_disconnected() {
    let d = dirs();
    runtime().block_on(async {
        let rig = Rig::start("pongs").await;
        let log = d.work.path().join("pongs.log");
        let (session, _) = rig.session("pinged", recorder(&log)).await;
        let (id, secret) = rig.link(&session, |_| {});
        let mut v = rig.viewer(&id, &secret, 80, 24).await;
        // All it was sent is acknowledged: no ack deadline runs for it.
        assert!(v.draws("RECORDING").await);
        let tcp = v.ws.get_mut();
        let pings: Vec<u8> = (0..512).flat_map(|_| frame(0x9, &[b'p'; 125])).collect();
        // Far past any honest buffer: 64 MiB of pongs owed to one viewer.
        let cap: u64 = 64 << 20;
        let mut written = 0u64;
        let dropped = loop {
            match shux_testkit::Deadline::start()
                .run(tcp.write_all(&pings))
                .await
            {
                Some(Ok(())) => written += pings.len() as u64,
                Some(Err(_)) => break true,
                None => panic!("the gateway stopped reading but kept the viewer connected"),
            }
            if written > cap {
                break false;
            }
        };
        assert!(
            dropped,
            "the gateway took {} MiB of pings from a viewer that read none of its pongs",
            written >> 20
        );
        rig.stop().await;
    });
}

/// A viewer on its own thread and runtime. It reads and acknowledges all it
/// is sent, so neither a stalled write nor the ack deadline ever drops it,
/// and once `go` opens it floods valid acks until it is cut off or stopped.
fn ack_flooder(
    addr: SocketAddr,
    id: GrantId,
    cookie: String,
    go: std::sync::Arc<tokio::sync::Barrier>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    cut: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) -> std::thread::JoinHandle<()> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("flooder runtime");
        rt.block_on(async move {
            let tcp = raw_upgrade(addr, &id, &cookie).await;
            let (mut rd, mut wr) = tcp.into_split();
            wr.write_all(&frame(0x1, br#"{"t":"hello","cols":80,"rows":24}"#))
                .await
                .expect("hello");
            let received = Arc::new(AtomicU64::new(0));
            let seen = received.clone();
            tokio::spawn(async move {
                loop {
                    let mut h = [0u8; 2];
                    if rd.read_exact(&mut h).await.is_err() {
                        return;
                    }
                    let len = match h[1] & 0x7f {
                        126 => {
                            let mut l = [0u8; 2];
                            if rd.read_exact(&mut l).await.is_err() {
                                return;
                            }
                            u64::from(u16::from_be_bytes(l))
                        }
                        127 => {
                            let mut l = [0u8; 8];
                            if rd.read_exact(&mut l).await.is_err() {
                                return;
                            }
                            u64::from_be_bytes(l)
                        }
                        n => u64::from(n),
                    };
                    let mut body = vec![0u8; usize::try_from(len).unwrap()];
                    if rd.read_exact(&mut body).await.is_err() {
                        return;
                    }
                    if h[0] & 0x0f == 0x2 {
                        let offset = u64::from_be_bytes(body[..8].try_into().unwrap());
                        seen.store(offset + len - 8, Ordering::SeqCst);
                    }
                }
            });
            go.wait().await;
            while !stop.load(Ordering::SeqCst) {
                let ack = format!(
                    r#"{{"t":"ack","offset":{}}}"#,
                    received.load(Ordering::SeqCst)
                );
                let burst: Vec<u8> = (0..256).flat_map(|_| frame(0x1, ack.as_bytes())).collect();
                if wr.write_all(&burst).await.is_err() {
                    cut.fetch_add(1, Ordering::SeqCst);
                    return;
                }
            }
        });
    })
}

#[test]
fn a_flood_of_viewer_messages_is_cut_off_and_never_starves_the_hosts_keys() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let d = dirs();
    runtime().block_on(async {
        let rig = Rig::start("ackflood").await;
        let log = d.work.path().join("ackflood.log");
        let (session, _) = rig.session("acked", recorder(&log)).await;
        let mut host = Host::attach(&rig, "acked", 100, 30).await;
        host.draws("RECORDING").await;
        let opened = log.clone();
        shux_testkit::wait_until_async("the recorder opens its log", || {
            let log = opened.clone();
            async move { log.exists() }
        })
        .await;
        // Every stream the daemon allows, one per link.
        let n = shux::share::MAX_STREAMS;
        let go = Arc::new(tokio::sync::Barrier::new(n + 1));
        let stop = Arc::new(AtomicBool::new(false));
        let cut = Arc::new(AtomicUsize::new(0));
        let mut threads = Vec::new();
        for _ in 0..n {
            let (id, secret) = rig.link(&session, |_| {});
            let cookie = rig.redeem(&id, &secret).await.cookie(&id);
            threads.push(ack_flooder(
                rig.addr,
                id,
                cookie,
                go.clone(),
                stop.clone(),
                cut.clone(),
            ));
        }
        go.wait().await;
        host.key(b"MARK").await;
        let landed = log.clone();
        shux_testkit::wait_until_async("the host's key reaches its pane", || {
            let log = landed.clone();
            async move { std::fs::read(&log).is_ok_and(|b| b == b"MARK") }
        })
        .await;
        let gone = cut.clone();
        shux_testkit::wait_until_async("every flooder is cut off", || {
            let gone = gone.clone();
            async move { gone.load(Ordering::SeqCst) == n }
        })
        .await;
        stop.store(true, Ordering::SeqCst);
        for t in threads {
            t.join().expect("flooder");
        }
        rig.stop().await;
    });
}

/// The status a GET answers with; None when the connection is closed unread.
async fn try_status(addr: SocketAddr, path: &str) -> Option<u16> {
    let mut tcp = TcpStream::connect(addr).await.ok()?;
    let req = format!("GET {path} HTTP/1.1\r\nHost: {HOST}\r\nConnection: close\r\n\r\n");
    tcp.write_all(req.as_bytes()).await.ok()?;
    let mut raw = Vec::new();
    tcp.read_to_end(&mut raw).await.ok()?;
    std::str::from_utf8(raw.get(9..12)?).ok()?.parse().ok()
}

#[test]
fn requests_that_never_finish_their_body_cannot_hold_every_connection() {
    let _ = dirs();
    runtime().block_on(async {
        let rig = Rig::start("bodies").await;
        let mut held = Vec::new();
        for _ in 0..64 {
            let mut tcp = TcpStream::connect(rig.addr).await.expect("connect");
            tcp.write_all(
                b"POST /v/abcdefgh/redeem HTTP/1.1\r\nHost: elsewhere\r\nContent-Length: 500\r\n\r\n{",
            )
            .await
            .expect("headers");
            held.push(tcp);
        }
        let addr = rig.addr;
        shux_testkit::wait_until_async("the gateway answers while 64 bodies stall", move || async move {
            try_status(addr, "/v/abcdefgh").await == Some(200)
        })
        .await;
        // Answered while the stalled requests still hold their places: one
        // was closed to make room, not all of them by their deadline.
        let mut buf = [0u8; 1];
        let open = held
            .iter()
            .filter(|t| matches!(t.try_read(&mut buf), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock))
            .count();
        assert!(open >= 60, "only {open} of 64 stalled requests were still open");
        drop(held);
        rig.stop().await;
    });
}

#[test]
fn browser_viewers_publish_no_client_events() {
    let _ = dirs();
    runtime().block_on(async {
        let rig = Rig::start("events").await;
        let (session, _) = rig.session("evented", producer()).await;
        let (id, secret) = rig.link(&session, |_| {});
        let cookie = rig.redeem(&id, &secret).await.cookie(&id);
        for _ in 0..3 {
            let mut v = Viewer::connect(rig.addr, &id, &cookie, ORIGIN, 80, 24)
                .await
                .unwrap();
            assert!(v.draws("TICK").await);
        }
        let history = rig
            .rpc("events.history", serde_json::json!({ "count": 1000 }))
            .await;
        let text = history.to_string();
        for kind in [
            "client.connected",
            "client.disconnected",
            "session.attached",
            "session.detached",
        ] {
            assert!(!text.contains(kind), "a browser viewer published {kind}");
        }
        rig.stop().await;
    });
}

#[test]
fn a_viewer_draws_at_the_hosts_layout_and_never_larger() {
    let d = dirs();
    runtime().block_on(async {
        let rig = Rig::start("clamp").await;
        let log = d.work.path().join("clamp.log");
        let (session, _) = rig.session("clamped", recorder(&log)).await;
        let mut host = Host::attach(&rig, "clamped", 80, 24).await;
        host.draws("RECORDING").await;
        let (id, secret) = rig.link(&session, |_| {});
        let mut v = rig.viewer(&id, &secret, 200, 60).await;
        assert_eq!(
            v.ready["host"],
            serde_json::json!({ "cols": 80, "rows": 23 })
        );
        assert_eq!(
            v.ready["size"],
            serde_json::json!({ "cols": 80, "rows": 23 }),
            "a grid larger than the host's"
        );
        v.send(serde_json::json!({ "t": "resize", "cols": 240, "rows": 100 }))
            .await;
        v.send(serde_json::json!({ "t": "resize", "cols": 40, "rows": 10 }))
            .await;
        while v.resizes.last() != Some(&(40, 10)) {
            assert!(v.step().await);
        }
        assert!(
            v.resizes.iter().all(|&(c, r)| c <= 80 && r <= 23),
            "{:?}",
            v.resizes
        );
        rig.stop().await;
    });
}

#[test]
fn with_no_host_a_viewer_is_told_the_size_that_shows_its_panes_whole() {
    let d = dirs();
    runtime().block_on(async {
        let rig = Rig::start("nohost").await;
        let log = d.work.path().join("nohost.log");
        let (session, _) = rig.session("hostless", recorder(&log)).await;
        let (id, secret) = rig.link(&session, |_| {});
        let mut v = rig.viewer(&id, &secret, 60, 12).await;
        let host = &v.ready["host"];
        assert!(
            host.is_object(),
            "no layout size for a hostless session: {}",
            v.ready
        );
        let (c, r) = (
            host["cols"].as_u64().unwrap(),
            host["rows"].as_u64().unwrap(),
        );
        v.send(serde_json::json!({ "t": "resize", "cols": c, "rows": r }))
            .await;
        // The pane's first row is shown, not cut off by its border.
        assert!(v.draws("TRUECOLOR").await && v.draws("RECORDING").await);
        rig.stop().await;
    });
}

#[test]
fn one_link_cannot_take_every_viewer_or_stream() {
    let _ = dirs();
    runtime().block_on(async {
        let rig = Rig::start("caps").await;
        let (session, _) = rig.session("capped", producer()).await;
        let (id, secret) = rig.link(&session, |o| o.uses = Uses::Unlimited);
        let mut codes = Vec::new();
        for _ in 0..40 {
            codes.push(rig.redeem(&id, &secret).await.status);
        }
        let ok = codes.iter().filter(|&&c| c == 200).count();
        assert!(ok < 40, "one link redeemed {ok} viewers");
        let (other, other_secret) = rig.link(&session, |_| {});
        assert_eq!(rig.redeem(&other, &other_secret).await.status, 200);
        // Its streams are capped too, below the daemon's.
        let (busy, busy_secret) = rig.link(&session, |o| o.uses = Uses::Unlimited);
        let mut live = Vec::new();
        let mut refused = 0;
        for _ in 0..shux::share::MAX_STREAMS {
            let cookie = rig.redeem(&busy, &busy_secret).await.cookie(&busy);
            match Viewer::connect(rig.addr, &busy, &cookie, ORIGIN, 80, 24).await {
                Ok(v) => live.push(v),
                Err(code) => {
                    assert_eq!(code, 503);
                    refused += 1;
                }
            }
        }
        assert!(refused > 0, "one link held all {} streams", live.len());
        rig.stop().await;
    });
}

#[test]
fn an_absolute_form_request_for_another_authority_is_not_served() {
    let _ = dirs();
    runtime().block_on(async {
        let rig = Rig::start("absolute").await;
        let (session, _) = rig.session("absolute", producer()).await;
        let (id, _) = rig.link(&session, |_| {});
        let path = format!("http://evil.test/v/{}", id.as_str());
        let r = http(rig.addr, "GET", &path, &[("Host", HOST)], "").await;
        assert_eq!(r.status, 404, "served a request for evil.test");
        rig.stop().await;
    });
}

/// What a server's frames carried: the binary payload of every complete
/// frame (offset prefix excluded), and its text frames.
fn parse_frames(raw: &[u8]) -> (u64, Vec<String>) {
    let (mut at, mut payload, mut texts) = (0usize, 0u64, Vec::new());
    while let Some(&[b0, b1]) = raw.get(at..at + 2) {
        let (mut len, mut head) = (u64::from(b1 & 0x7f), 2usize);
        if len == 126 {
            let Some(l) = raw.get(at + 2..at + 4) else {
                break;
            };
            len = u64::from(u16::from_be_bytes(l.try_into().unwrap()));
            head = 4;
        } else if len == 127 {
            let Some(l) = raw.get(at + 2..at + 10) else {
                break;
            };
            len = u64::from_be_bytes(l.try_into().unwrap());
            head = 10;
        }
        let end = at + head + usize::try_from(len).unwrap();
        let Some(body) = raw.get(at + head..end) else {
            break;
        };
        match b0 & 0x0f {
            0x2 => payload += len - 8,
            0x1 => texts.push(String::from_utf8_lossy(body).to_string()),
            _ => {}
        }
        at = end;
    }
    (payload, texts)
}

#[test]
fn a_revoked_viewer_gets_no_terminal_bytes_past_the_cutoff() {
    let _ = dirs();
    runtime().block_on(async {
        let rig = Rig::start("cutoff").await;
        let flood = serde_json::json!([
            "sh",
            "-c",
            "i=0; while :; do i=$((i+1)); printf \"\\033[3%dmTICK %s %0200d\\033[0m\\n\" $((i%7+1)) $i 0; done"
        ]);
        let (session, _) = rig.session("cutoff", flood).await;
        let mut host = Host::attach(&rig, "cutoff", 240, 101).await;
        host.draws("TICK").await;
        let (id, secret) = rig.link(&session, |_| {});
        let cookie = rig.redeem(&id, &secret).await.cookie(&id);
        // A viewer whose socket holds almost nothing and which reads nothing:
        // the origin's write blocks partway through a chunk.
        let sock = tokio::net::TcpSocket::new_v4().expect("socket");
        sock.set_recv_buffer_size(4096).expect("rcvbuf");
        let tcp = sock.connect(rig.addr).await.expect("connect");
        let mut tcp = raw_upgrade_on(tcp, &id, &cookie).await;
        tcp.write_all(&frame(0x1, br#"{"t":"hello","cols":240,"rows":100}"#))
            .await
            .expect("hello");
        // Stuck: what the origin has fully sent holds still while the pane
        // keeps printing and the host keeps drawing it. A full window held
        // back is credit, not a blocked write: acknowledge it (unread) until
        // the origin stops short of the window, its write blocked on the
        // socket.
        let mut acked = 0;
        let cut = loop {
            let before = rig.gw.bytes_sent();
            let seen = host.frames();
            host.frames_past(seen + 10).await;
            let now = rig.gw.bytes_sent();
            if now != before || now == 0 {
                continue;
            }
            if now - acked < shux::share::wire::WINDOW {
                break now;
            }
            acked = now;
            let ack = format!(r#"{{"t":"ack","offset":{acked}}}"#);
            tcp.write_all(&frame(0x1, ack.as_bytes())).await.expect("ack");
        };
        // Read alongside the revoke, so anything the origin still writes
        // gets through.
        let reader = tokio::spawn(async move {
            let mut raw = Vec::new();
            let _ = tokio::time::timeout(BUDGET, tcp.read_to_end(&mut raw)).await;
            raw
        });
        rig.gw.revoke(&id).await.expect("revoke");
        let raw = reader.await.expect("reader");
        let (payload, _) = parse_frames(&raw);
        assert!(
            payload <= cut,
            "{payload} terminal bytes reached the viewer; the origin had sent {cut} before the cutoff"
        );
        rig.stop().await;
    });
}

#[test]
fn administrative_detach_invalidates_the_browser_credential() {
    let _ = dirs();
    runtime().block_on(async {
        let rig = Rig::start("adm-detach").await;
        let (session, _) = rig.session("adm-detach", producer()).await;
        let (id, secret) = rig.link(&session, |_| {});
        let cookie = rig.redeem(&id, &secret).await.cookie(&id);
        let mut viewer = Viewer::connect(rig.addr, &id, &cookie, ORIGIN, 80, 24)
            .await
            .unwrap();
        assert!(viewer.draws("TICK").await);
        let listed = rig.rpc("client.list", serde_json::json!({})).await;
        let clients = listed["clients"].as_array().unwrap();
        assert_eq!(clients.len(), 1);
        assert_eq!(clients[0]["read_only"], true);
        rig.rpc(
            "client.detach",
            serde_json::json!({ "id": clients[0]["id"] }),
        )
        .await;
        // Refused the moment the registry lets it go.
        assert_eq!(
            upgrade(rig.addr, &id, &cookie, ORIGIN).await.err(),
            Some(403)
        );
        let status = http(
            rig.addr,
            "GET",
            &format!("/v/{}/status", id.as_str()),
            &[("Host", HOST), ("Cookie", &cookie)],
            "",
        )
        .await;
        assert_eq!(status.status, 410);
        assert_eq!(viewer.ends().await.as_deref(), Some("kicked"));
        rig.stop().await;
    });
}

#[test]
fn a_hostless_split_shows_each_pane_whole() {
    let _ = dirs();
    runtime().block_on(async {
        let rig = Rig::start("hostless-split").await;
        let wide = serde_json::json!(["sh", "-c", "printf '%090dEDGE%sA\\n' 0 ''; exec cat"]);
        let (session, left) = rig.session("split", wide).await;
        let split = rig
            .rpc(
                "pane.split",
                serde_json::json!({ "pane_id": left, "direction": "vertical", "command": ["sh", "-c", "printf 'NAR%sROW\\n' ''; exec cat"] }),
            )
            .await;
        let right = split["pane"]["id"].as_str().expect("the new pane").to_string();
        // Panes of very different widths, with nothing attached to size them.
        for (pane, cols, rows) in [(&left, 100, 30), (&right, 30, 10)] {
            rig.rpc(
                "pane.set_size",
                serde_json::json!({ "pane_id": pane, "cols": cols, "rows": rows }),
            )
            .await;
        }
        let (id, secret) = rig.link(&session, |_| {});
        let mut v = rig.viewer(&id, &secret, 60, 20).await;
        let host = v.ready["host"].clone();
        v.send(serde_json::json!({ "t": "resize", "cols": host["cols"], "rows": host["rows"] }))
            .await;
        assert!(v.draws("NARROW").await, "the narrow pane never drew");
        assert!(v.draws("EDGEA").await, "the wide pane was cropped");
        rig.stop().await;
    });
}

#[test]
fn failed_redemptions_are_rate_limited_per_client_and_overall() {
    let _ = dirs();
    runtime().block_on(async {
        let rig = Rig::start("guesses").await;
        let (session, _) = rig.session("guessed", producer()).await;
        let (id, secret) = rig.link(&session, |o| o.uses = Uses::Unlimited);
        let attempt = |client: String, secret: String| {
            let (addr, id) = (rig.addr, id.clone());
            async move {
                let body = serde_json::json!({ "secret": secret }).to_string();
                http(
                    addr,
                    "POST",
                    &format!("/v/{}/redeem", id.as_str()),
                    &[
                        ("Host", HOST),
                        ("Origin", ORIGIN),
                        ("Content-Type", "application/json"),
                        ("CF-Connecting-IP", &client),
                    ],
                    &body,
                )
                .await
                .status
            }
        };
        let wrong = "A".repeat(43);
        let mut codes = Vec::new();
        for _ in 0..20 {
            codes.push(attempt("192.0.2.1".into(), wrong.clone()).await);
        }
        assert!(codes.contains(&429), "one client guessed freely: {codes:?}");
        assert_eq!(
            attempt("192.0.2.1".into(), secret.clone()).await,
            429,
            "a client over its budget is answered without the secret being tried"
        );
        assert_eq!(
            attempt("192.0.2.2".into(), secret.clone()).await,
            200,
            "another client is not held back"
        );
        // Many clients, each under its own budget, exhaust the global one.
        let mut overall = Vec::new();
        for n in 0..400u32 {
            overall.push(attempt(format!("198.51.{}.{}", n / 250, n % 250), wrong.clone()).await);
        }
        assert!(
            overall.contains(&429),
            "guesses spread over clients were never limited"
        );
        rig.stop().await;
    });
}
