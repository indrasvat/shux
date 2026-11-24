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
        gw.set_origin(Origin::parse(ORIGIN).unwrap()).await;
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

        // Resizes reach this viewer's own grid, held to the browser's bound.
        for (cols, rows) in [(50, 15), (61, 17), (300, 300)] {
            viewer
                .send(serde_json::json!({ "t": "resize", "cols": cols, "rows": rows }))
                .await;
        }
        while viewer.resizes.last() != Some(&(240, 100)) {
            assert!(viewer.step().await);
        }
        assert!(
            viewer.resizes.iter().all(|&(c, r)| c <= 240 && r <= 100),
            "{:?}",
            viewer.resizes
        );
        assert!(
            viewer.draws("RECORDING").await,
            "nothing drawn at the new size"
        );
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
        // Neither the live viewer nor the offline one gets back in.
        for cookie in [&offline] {
            assert_eq!(
                upgrade(rig.addr, &id, cookie, ORIGIN).await.err(),
                Some(403)
            );
            let status = http(
                rig.addr,
                "GET",
                &format!("/v/{}/status", id.as_str()),
                &[("Host", HOST), ("Cookie", cookie)],
                "",
            )
            .await;
            assert_eq!(status.status, 410);
        }
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
        let (session, _) = rig.session("flooded", producer()).await;
        let (id, secret) = rig.link(&session, |_| {});
        let r = rig.redeem(&id, &secret).await;
        let cookie = r.cookie(&id);
        let mut stalled = Viewer::connect(rig.addr, &id, &cookie, ORIGIN, 80, 24)
            .await
            .unwrap();
        stalled.acks = false;
        assert_eq!(stalled.ends().await.as_deref(), Some("lagging"));
        assert!(
            stalled.received <= shux::share::wire::WINDOW,
            "{} bytes reached a viewer that acknowledged none",
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
    let _ = dirs();
    runtime().block_on(async {
        let rig = Rig::start("ends").await;
        let (session, _) = rig.session("ended", producer()).await;
        let (id, secret) = rig.link(&session, |o| o.uses = Uses::Unlimited);
        let mut kicked = rig.viewer(&id, &secret, 80, 24).await;
        assert!(kicked.draws("TICK").await);
        let viewer = rig.gw.list()[0].viewers[0].id.clone();
        rig.gw.kick(&viewer).await.expect("kick");
        assert_eq!(kicked.ends().await.as_deref(), Some("kicked"));
        assert_eq!(rig.gw.list()[0].viewers.len(), 0);

        let (short, short_secret) =
            rig.link(&session, |o| o.ttl = std::time::Duration::from_secs(1));
        let mut expiring = rig.viewer(&short, &short_secret, 80, 24).await;
        assert_eq!(expiring.ends().await.as_deref(), Some("expired"));

        let mut stopped = rig.viewer(&id, &secret, 80, 24).await;
        assert!(stopped.draws("TICK").await);
        rig.gw.stop().await;
        assert_eq!(stopped.ends().await.as_deref(), Some("stopped"));
        assert!(rig.gw.list().is_empty());
        assert!(
            TcpStream::connect(rig.addr).await.is_err(),
            "the listener outlived stop"
        );
        rig.gw.stop().await;
        rig.stop().await;
    });
}
