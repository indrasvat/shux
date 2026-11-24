//! The public half of `shux share`: one Cloudflare quick tunnel per daemon.
//!
//! The daemon execs `shux __share-supervise` with two descriptors: stdin, the
//! liveness pipe whose EOF means the daemon is done or dead, and stderr, a
//! duplicate of the gateway's bound listener. The supervisor runs cloudflared
//! in a process group of its own with neither, a private `HOME`, its own
//! config and a bare environment. It watches cloudflared's pid, not its log:
//! told to stop, or once cloudflared exits, it kills the whole group, reaps
//! it, and only then lets the port go, so after a daemon `SIGKILL` no other
//! process can take the traffic the tunnel still carries. It reads
//! cloudflared's log itself and tells the daemon only [`Report`]s.
//!
//! A link is issued only once a marker for this tunnel's generation has come
//! back through the exact public origin, over HTTPS, with no redirect.

use std::io::{BufRead, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::BodyExt;
use tokio::io::AsyncBufReadExt;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::Origin;

pub const QUICK_DOMAIN: &str = "trycloudflare.com";
const MAX_LINE: usize = 4096;
/// From exec to the startup record. cloudflared takes about 4 s.
const STARTUP: Duration = Duration::from_secs(30);
/// From the startup record to the marker answering at the public origin.
const REACHABLE: Duration = Duration::from_secs(60);
/// How long a link waits for every edge address before one is enough.
const EVERYWHERE: Duration = Duration::from_secs(30);
const PROBE_ATTEMPT: Duration = Duration::from_secs(5);
const PROBE_PAUSE: Duration = Duration::from_millis(750);
/// How long a supervisor has to reap cloudflared once told to stop.
const REAP: Duration = Duration::from_secs(10);
pub const PROBE_PATH: &str = "/.well-known/shux-probe";
pub const INSTALL_HINT: &str = "install cloudflared (macOS: `brew install cloudflared`; \
    elsewhere: https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/), \
    or point SHUX_CLOUDFLARED at it";

/// Why a tunnel did not come up, or went down, as a stable code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TunnelError {
    #[error("cloudflared was not found: {INSTALL_HINT}")]
    NoCloudflared,
    #[error("the share supervisor could not start")]
    SpawnFailed,
    #[error("cloudflared exited before its tunnel was ready")]
    Exited,
    #[error(
        "Cloudflare is refusing new quick tunnels from this network for now (429); try again in a few minutes"
    )]
    RateLimited,
    #[error("cloudflared gave no quick-tunnel address in {}s", STARTUP.as_secs())]
    Timeout,
    #[error("the tunnel's address never answered for this share within {}s", REACHABLE.as_secs())]
    Unreachable,
    #[error("sharing was stopped while the tunnel was starting")]
    Cancelled,
}

impl TunnelError {
    pub fn code(self) -> &'static str {
        match self {
            Self::NoCloudflared => "no_cloudflared",
            Self::SpawnFailed => "spawn_failed",
            Self::Exited => "exited",
            Self::RateLimited => "rate_limited",
            Self::Timeout => "timeout",
            Self::Unreachable => "unreachable",
            Self::Cancelled => "cancelled",
        }
    }
}

/// `https://<one label>.trycloudflare.com` and nothing else.
pub fn quick_origin(url: &str) -> Option<Origin> {
    let origin = Origin::parse(url).ok()?;
    let label = origin
        .host()
        .strip_suffix(QUICK_DOMAIN)?
        .strip_suffix('.')?;
    let ok = !label.is_empty()
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    ok.then_some(origin)
}

/// One line of the supervisor's report to the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    /// The process group cloudflared runs in, for a daemon that must kill it
    /// past its supervisor.
    Group(i32),
    Origin(Origin),
    /// Cloudflare refused to make a tunnel: too many from this network.
    RateLimited,
    /// The supervisor is done, for this reason.
    Exit(Exit),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// cloudflared ended by itself.
    Exited,
    /// The daemon closed the liveness pipe, or went away.
    Stopped,
    /// cloudflared could not be started, or the listener was not handed over.
    SpawnFailed,
}

impl Exit {
    pub fn code(self) -> &'static str {
        match self {
            Self::Exited => "exited",
            Self::Stopped => "stopped",
            Self::SpawnFailed => "spawn_failed",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        [Self::Exited, Self::Stopped, Self::SpawnFailed]
            .into_iter()
            .find(|e| e.code() == s)
    }
}

impl Report {
    pub fn line(&self) -> String {
        match self {
            Self::Group(g) => format!("group {g}"),
            Self::Origin(o) => format!("origin {}", o.as_str()),
            Self::RateLimited => "stage rate_limited".into(),
            Self::Exit(e) => format!("exit {}", e.code()),
        }
    }
    /// The daemon reads nothing else: an unknown line is `None`.
    pub fn parse(line: &str) -> Option<Self> {
        match line.trim_end().split_once(' ')? {
            ("group", g) => g.parse().ok().filter(|g| *g > 1).map(Self::Group),
            ("stage", "rate_limited") => Some(Self::RateLimited),
            ("origin", url) => quick_origin(url).map(Self::Origin),
            ("exit", code) => Exit::parse(code).map(Self::Exit),
            _ => None,
        }
    }
}

/// Reads cloudflared's log for its startup record: the boxed line right after
/// "Your quick Tunnel has been created", holding only the URL.
#[derive(Debug, Default)]
pub struct StartupParser {
    armed: bool,
    origin: bool,
}

impl StartupParser {
    pub fn feed(&mut self, line: &str) -> Option<Report> {
        if self.origin {
            return None;
        }
        if line.to_ascii_lowercase().contains("quick tunnel") && line.contains("status 429") {
            return Some(Report::RateLimited);
        }
        if line.contains("Your quick Tunnel has been created") {
            self.armed = true;
            return None;
        }
        if !std::mem::take(&mut self.armed) {
            return None;
        }
        let (_, boxed) = line.split_once('|')?;
        let url = boxed
            .strip_suffix('|')
            .or_else(|| boxed.trim_end().strip_suffix('|'))?;
        let origin = quick_origin(url.trim())?;
        self.origin = true;
        Some(Report::Origin(origin))
    }
}

// ── The supervisor: `shux __share-supervise` ─────────────────────────────────

/// Takes the listener on stderr, which must be the gateway's on loopback at
/// `port`. The copy held is close-on-exec.
fn hold_listener(port: u16) -> Option<std::net::TcpListener> {
    let owned = std::io::stderr().as_fd().try_clone_to_owned().ok()?;
    let listener = std::net::TcpListener::from(owned);
    let addr = listener.local_addr().ok()?;
    (addr.ip().is_loopback() && addr.port() == port).then_some(listener)
}

enum Msg {
    Line(String),
    Exited,
    Stop,
}

/// Reads one line of at most [`MAX_LINE`] bytes; a longer one is skipped, its
/// remainder discarded as it arrives, never held.
pub fn bounded_line(r: &mut impl BufRead, buf: &mut Vec<u8>) -> std::io::Result<Option<String>> {
    loop {
        buf.clear();
        let n = std::io::Read::take(&mut *r, MAX_LINE as u64).read_until(b'\n', buf)?;
        if n == 0 {
            return Ok(None);
        }
        if buf.ends_with(b"\n") || n < MAX_LINE {
            return Ok(Some(String::from_utf8_lossy(buf).into_owned()));
        }
        loop {
            let chunk = r.fill_buf()?;
            if chunk.is_empty() {
                return Ok(None);
            }
            match chunk.iter().position(|&b| b == b'\n') {
                Some(i) => {
                    r.consume(i + 1);
                    break;
                }
                None => {
                    let len = chunk.len();
                    r.consume(len);
                }
            }
        }
    }
}

/// Kills cloudflared's whole process group.
fn kill_group(group: i32) {
    let _ = nix::sys::signal::killpg(
        nix::unistd::Pid::from_raw(group),
        nix::sys::signal::Signal::SIGKILL,
    );
}

/// Runs cloudflared for the gateway on `127.0.0.1:port` until stdin closes
/// or cloudflared exits, reporting on stdout. Returns once cloudflared's
/// group is killed and cloudflared reaped; the listener is released only then.
pub fn supervise(cloudflared: &Path, port: u16) -> anyhow::Result<()> {
    let mut out = std::io::stdout().lock();
    let mut report = |r: Report| -> bool {
        writeln!(out, "{}", r.line())
            .and_then(|()| out.flush())
            .is_ok()
    };
    let Some(listener) = hold_listener(port) else {
        report(Report::Exit(Exit::SpawnFailed));
        return Ok(());
    };
    let home = tempfile::Builder::new().prefix("shux-share-").tempdir()?;
    let config = home.path().join("config.yml");
    // Ours, so no config.yml elsewhere on the machine can name a tunnel.
    std::fs::write(&config, "no-autoupdate: true\n")?;
    let spawned = std::process::Command::new(cloudflared)
        .args(["tunnel", "--no-autoupdate", "--config"])
        .arg(&config)
        .args(["--loglevel", "info", "--metrics", "127.0.0.1:0", "--url"])
        .arg(format!("http://127.0.0.1:{port}"))
        .env_clear()
        .env("HOME", home.path())
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .current_dir(home.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .process_group(0)
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(_) => {
            report(Report::Exit(Exit::SpawnFailed));
            return Ok(());
        }
    };
    // Its pid is its group: it leads the group it was spawned into.
    let group = child.id() as i32;
    let (tx, rx) = std::sync::mpsc::channel();
    if let Some(log) = child.stderr.take() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut log = std::io::BufReader::new(log);
            let mut buf = Vec::new();
            // A closed log means nothing: the pid says whether it runs.
            while let Ok(Some(line)) = bounded_line(&mut log, &mut buf) {
                if tx.send(Msg::Line(line)).is_err() {
                    return;
                }
            }
        });
    }
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let status = child.wait();
            let _ = tx.send(Msg::Exited);
            let _ = done_tx.send(status);
        });
    }
    std::thread::spawn(move || {
        // Anything from the daemon, or nothing ever again, means stop.
        let mut byte = [0u8; 1];
        let _ = std::io::Read::read(&mut std::io::stdin().lock(), &mut byte);
        let _ = tx.send(Msg::Stop);
    });
    let mut alive = report(Report::Group(group));
    let mut parser = StartupParser::default();
    let why = loop {
        if !alive {
            break Exit::Stopped;
        }
        match rx.recv() {
            Ok(Msg::Line(line)) => {
                if let Some(r) = parser.feed(&line) {
                    alive &= report(r);
                }
            }
            Ok(Msg::Exited) => break Exit::Exited,
            Ok(Msg::Stop) | Err(_) => break Exit::Stopped,
        }
    };
    // Whatever it left behind in its group goes with it.
    kill_group(group);
    let _ = done_rx.recv();
    kill_group(group);
    report(Report::Exit(why));
    drop(listener);
    drop(home);
    Ok(())
}

// ── The daemon's side ────────────────────────────────────────────────────────

/// cloudflared: `SHUX_CLOUDFLARED`, else the first on `PATH`, else where
/// the common installers put it.
pub fn find_cloudflared() -> Result<PathBuf, TunnelError> {
    let usable = |p: &Path| {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if let Some(p) = std::env::var_os("SHUX_CLOUDFLARED") {
        let p = PathBuf::from(p);
        return (p.is_absolute() && usable(&p))
            .then_some(p)
            .ok_or(TunnelError::NoCloudflared);
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from))
        .filter(|d| d.is_absolute())
        .map(|d| d.join("cloudflared"))
        .find(|p| usable(p))
        .ok_or(TunnelError::NoCloudflared)
}

/// How the marker is fetched back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    /// HTTPS to the public origin by its name (SNI, certificate and `Host`),
    /// on 443 at the quick-tunnel edge, the OS's roots trusted.
    Public,
    /// Plain HTTP to this address, naming the public host: for tests whose
    /// cloudflared is a stand-in with no edge behind it.
    Loopback(std::net::SocketAddr),
}

/// The exec'd supervisor, from the daemon's side.
pub struct Supervisor {
    child: tokio::process::Child,
    /// Dropped to tell it to stop.
    liveness: Option<tokio::process::ChildStdin>,
    reports: Option<tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>>,
    /// cloudflared's process group, once reported.
    group: Option<i32>,
}

impl Supervisor {
    /// Execs `shux` as the supervisor of `cloudflared` for the gateway
    /// listening on `listener`, at `port`.
    pub fn spawn(
        shux: &Path,
        cloudflared: &Path,
        listener: &OwnedFd,
        port: u16,
    ) -> Result<Self, TunnelError> {
        let held = listener.try_clone().map_err(|_| TunnelError::SpawnFailed)?;
        let mut child = tokio::process::Command::new(shux)
            .arg("__share-supervise")
            .arg("--cloudflared")
            .arg(cloudflared)
            .arg("--port")
            .arg(port.to_string())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            // The listener, for it to hold until cloudflared is reaped.
            .stderr(std::process::Stdio::from(held))
            .spawn()
            .map_err(|_| TunnelError::SpawnFailed)?;
        let liveness = child.stdin.take();
        let reports = child
            .stdout
            .take()
            .map(|o| tokio::io::BufReader::new(o).lines());
        Ok(Self {
            child,
            liveness,
            reports,
            group: None,
        })
    }

    /// The next report it understands; `None` once it has said its last.
    pub async fn next(&mut self) -> Option<Report> {
        let reports = self.reports.as_mut()?;
        loop {
            match reports.next_line().await {
                Ok(Some(line)) => match Report::parse(&line) {
                    Some(Report::Group(g)) => self.group = Some(g),
                    Some(r) => return Some(r),
                    None => {}
                },
                _ => {
                    self.reports = None;
                    return None;
                }
            }
        }
    }

    /// Waits for the startup record, racing cloudflared's exit and `cancel`.
    pub async fn origin(&mut self, cancel: &CancellationToken) -> Result<Origin, TunnelError> {
        let wait = async {
            let mut refused = false;
            loop {
                match self.next().await {
                    Some(Report::Origin(o)) => return Ok(o),
                    Some(Report::RateLimited) => refused = true,
                    Some(Report::Exit(Exit::SpawnFailed)) => return Err(TunnelError::SpawnFailed),
                    Some(Report::Exit(_)) | None if refused => {
                        return Err(TunnelError::RateLimited);
                    }
                    Some(Report::Exit(_)) | None => return Err(TunnelError::Exited),
                    Some(Report::Group(_)) => {}
                }
            }
        };
        tokio::select! {
            () = cancel.cancelled() => Err(TunnelError::Cancelled),
            r = tokio::time::timeout(STARTUP, wait) => r.unwrap_or(Err(TunnelError::Timeout)),
        }
    }

    /// Fires once it has stopped reporting: cloudflared ended, or it did.
    pub fn watch(&mut self) -> CancellationToken {
        let lost = CancellationToken::new();
        if let Some(mut reports) = self.reports.take() {
            let lost = lost.clone();
            tokio::spawn(async move {
                while let Ok(Some(line)) = reports.next_line().await {
                    if matches!(Report::parse(&line), Some(Report::Exit(_))) {
                        break;
                    }
                }
                lost.cancel();
            });
        } else {
            lost.cancel();
        }
        lost
    }

    /// Tells it to stop and waits for it, which it does once cloudflared's
    /// group is gone; past [`REAP`] the daemon kills the group and it.
    pub async fn stop(mut self) {
        drop(self.liveness.take());
        if tokio::time::timeout(REAP, self.child.wait()).await.is_err() {
            tracing::warn!("share: the supervisor outlived its stop; killing it and cloudflared");
            if let Some(g) = self.group {
                kill_group(g);
            }
            let _ = self.child.kill().await;
        }
    }
}

/// A fresh marker for one tunnel generation.
pub fn marker() -> Result<String, getrandom::Error> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b)?;
    Ok(b.iter().map(|b| format!("{b:02x}")).collect())
}

/// A TLS client trusting the OS's roots; `None` when it has none.
fn tls() -> Option<tokio_rustls::TlsConnector> {
    let mut roots = rustls::RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    let (added, _) = roots.add_parsable_certificates(native.certs);
    if added == 0 {
        return None;
    }
    let config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .ok()?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Some(tokio_rustls::TlsConnector::from(std::sync::Arc::new(
        config,
    )))
}

async fn get<IO>(io: IO, host: &str) -> Option<(u16, Bytes)>
where
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut tx, conn) = hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(io))
        .await
        .ok()?;
    let conn = tokio::spawn(conn);
    let req = hyper::Request::get(PROBE_PATH)
        .header(hyper::header::HOST, host)
        .header(hyper::header::USER_AGENT, "shux-share-probe")
        .body(http_body_util::Empty::<Bytes>::new())
        .ok()?;
    let answer = async {
        let res = tx.send_request(req).await.ok()?;
        let status = res.status().as_u16();
        let body = http_body_util::Limited::new(res.into_body(), 256)
            .collect()
            .await
            .ok()?
            .to_bytes();
        Some((status, body))
    }
    .await;
    conn.abort();
    answer
}

/// One fetch of the probe path over `tcp`: its status and body.
async fn fetch(
    tcp: tokio::net::TcpStream,
    tls: Option<&tokio_rustls::TlsConnector>,
    host: &str,
) -> Option<(u16, Bytes)> {
    let Some(tls) = tls else {
        return get(tcp, host).await;
    };
    let name = rustls::pki_types::ServerName::try_from(host.to_string()).ok()?;
    let stream = tls.connect(name, tcp).await.ok()?;
    get(stream, host).await
}

#[expect(
    clippy::disallowed_methods,
    reason = "the pause between probes of a tunnel still coming up; no test waits on it"
)]
async fn pause() {
    tokio::time::sleep(PROBE_PAUSE).await;
}

/// What one probe found: of the edge addresses this machine reached, how
/// many served exactly the marker at the probe path, with `200`. A redirect
/// is not followed; it is a no.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Answers {
    pub reached: usize,
    pub answered: usize,
}

impl Answers {
    pub fn everywhere(self) -> bool {
        self.answered > 0 && self.answered == self.reached
    }
    pub fn somewhere(self) -> bool {
        self.answered > 0
    }
}

/// One probe of `origin` for `marker`, given up at `deadline`.
///
/// The edge serves every quick tunnel at the addresses its apex resolves to,
/// choosing by name; asking for the new name itself before it exists would
/// cache "no such name" for a minute. A fresh name can answer at one of those
/// addresses and not yet another (Cloudflare's 1016), so each is asked.
async fn answers(
    probe: Probe,
    tls: Option<&tokio_rustls::TlsConnector>,
    origin: &Origin,
    marker: &str,
    deadline: Option<Instant>,
) -> Answers {
    let addrs: Vec<std::net::SocketAddr> = match probe {
        Probe::Loopback(addr) => vec![addr],
        Probe::Public => match tokio::net::lookup_host((QUICK_DOMAIN, 443)).await {
            Ok(a) => a.collect(),
            Err(_) => return Answers::default(),
        },
    };
    answers_at(&addrs, tls, origin, marker, deadline).await
}

/// One attempt's budget: [`PROBE_ATTEMPT`], or less to keep to `deadline`.
fn left(deadline: Option<Instant>) -> Duration {
    deadline.map_or(PROBE_ATTEMPT, |d| {
        PROBE_ATTEMPT.min(d.saturating_duration_since(Instant::now()))
    })
}

async fn answers_at(
    addrs: &[std::net::SocketAddr],
    tls: Option<&tokio_rustls::TlsConnector>,
    origin: &Origin,
    marker: &str,
    deadline: Option<Instant>,
) -> Answers {
    let mut found = Answers::default();
    for &addr in addrs {
        // Each connection is bounded by the attempt and by the deadline.
        let budget = left(deadline);
        if budget.is_zero() {
            break;
        }
        // An address with no route from here (often IPv6) cannot be checked.
        let Ok(Ok(tcp)) = tokio::time::timeout(budget, tokio::net::TcpStream::connect(addr)).await
        else {
            continue;
        };
        found.reached += 1;
        let budget = left(deadline);
        let got = tokio::time::timeout(budget, fetch(tcp, tls, origin.host()))
            .await
            .ok()
            .flatten();
        if got.is_some_and(|(status, body)| status == 200 && body == marker.as_bytes()) {
            found.answered += 1;
        }
    }
    found
}

/// Waits until every reachable edge address serves the marker, so no guest
/// meets an error page. Past [`EVERYWHERE`] a link is issued once any one
/// does: the origin reaches this gateway, and the rest of the edge follows
/// within a minute. Past [`REACHABLE`] with none, the tunnel failed.
pub async fn verify(
    probe: Probe,
    origin: &Origin,
    marker: &str,
    cancel: &CancellationToken,
) -> Result<(), TunnelError> {
    let connector = match probe {
        Probe::Public => Some(tls().ok_or(TunnelError::Unreachable)?),
        Probe::Loopback(_) => None,
    };
    let start = Instant::now();
    let deadline = start + REACHABLE;
    loop {
        let found = tokio::select! {
            () = cancel.cancelled() => return Err(TunnelError::Cancelled),
            a = answers(probe, connector.as_ref(), origin, marker, Some(deadline)) => a,
        };
        if found.everywhere() || (found.somewhere() && start.elapsed() >= EVERYWHERE) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(TunnelError::Unreachable);
        }
        tokio::select! {
            () = cancel.cancelled() => return Err(TunnelError::Cancelled),
            () = pause() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORD: &str = "\
2026-11-24T07:40:53Z INF Thank you for trying Cloudflare Tunnel. See https://www.cloudflare.com/website-terms/
2026-11-24T07:40:53Z INF Requesting new quick Tunnel on trycloudflare.com...
2026-11-24T07:40:57Z INF +--------------------------------------------------------------------------------------------+
2026-11-24T07:40:57Z INF |  Your quick Tunnel has been created! Visit it at (it may take some time to be reachable):  |
2026-11-24T07:40:57Z INF |  https://monroe-ali-chose-launch.trycloudflare.com                                         |
2026-11-24T07:40:57Z INF +--------------------------------------------------------------------------------------------+
2026-11-24T07:40:57Z INF Settings: map[ha-connections:1 no-autoupdate:true url:http://127.0.0.1:7791]
2026-11-24T07:40:58Z INF Registered tunnel connection connIndex=0 connection=abc event=0 ip=198.41.200.13 location=sjc06 protocol=quic
";

    fn reports(log: &str) -> Vec<Report> {
        let mut p = StartupParser::default();
        log.lines().filter_map(|l| p.feed(l)).collect()
    }

    #[test]
    fn the_startup_record_gives_the_origin_once() {
        assert_eq!(
            reports(&format!("{RECORD}{RECORD}")),
            vec![Report::Origin(
                Origin::parse("https://monroe-ali-chose-launch.trycloudflare.com").unwrap()
            )]
        );
    }

    #[test]
    fn only_the_line_after_the_record_header_is_read_for_the_origin() {
        // An address anywhere else in the log is never the origin.
        let stray = "INF |  https://evil.trycloudflare.com  |\n\
                     INF |  Your quick Tunnel has been created! Visit it at:  |\n\
                     INF +------------------------------+\n\
                     INF |  https://late.trycloudflare.com  |\n";
        assert_eq!(reports(stray), vec![]);
        // Nor is a URL that is not alone in its box.
        let crowded = "INF |  Your quick Tunnel has been created!  |\n\
                       INF |  https://a.trycloudflare.com and more  |\n";
        assert_eq!(reports(crowded), vec![]);
    }

    #[test]
    fn only_a_single_label_under_trycloudflare_is_an_origin() {
        assert!(quick_origin("https://monroe-ali-chose-launch.trycloudflare.com").is_some());
        for bad in [
            "http://a.trycloudflare.com",
            "https://trycloudflare.com",
            "https://.trycloudflare.com",
            "https://a.b.trycloudflare.com",
            "https://a.trycloudflare.com.evil.com",
            "https://evil.com@a.trycloudflare.com",
            "https://atrycloudflare.com",
            "https://a.trycloudflare.com:8443",
            "https://a.trycloudflare.com/v",
            "https://-a.trycloudflare.com",
            "https://a_b.trycloudflare.com",
            "https://a.trycloudflare.co",
        ] {
            assert!(quick_origin(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn a_refused_request_is_read_as_rate_limited() {
        let log = "2026-11-24T14:22:48Z INF Requesting new quick Tunnel on trycloudflare.com...\n\
                   quick tunnel provisioning failed with status 429: [10000] Tunnelstore request failed\n";
        assert_eq!(reports(log), vec![Report::RateLimited]);
    }

    #[test]
    fn a_report_round_trips_and_nothing_else_parses() {
        for r in [
            Report::Group(4242),
            Report::RateLimited,
            Report::Origin(Origin::parse("https://x-y.trycloudflare.com").unwrap()),
            Report::Exit(Exit::Exited),
            Report::Exit(Exit::Stopped),
            Report::Exit(Exit::SpawnFailed),
        ] {
            assert_eq!(Report::parse(&r.line()), Some(r.clone()), "{r:?}");
        }
        for junk in [
            "",
            "origin https://x.example.com",
            "exit crashed",
            "stage done",
            // Never a group that would reach every process, or init's.
            "group 0",
            "group 1",
            "group -1",
            "2026 INF Registered tunnel connection",
        ] {
            assert_eq!(Report::parse(junk), None, "{junk:?}");
        }
    }

    /// An origin that answers every request with `response`, and records the
    /// `Host` each one named.
    async fn canned(
        response: &'static str,
    ) -> (std::net::SocketAddr, tokio::sync::mpsc::Receiver<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                let mut req = vec![0u8; 2048];
                let n = s.read(&mut req).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&req[..n]).to_ascii_lowercase();
                let host = req
                    .lines()
                    .find_map(|l| l.strip_prefix("host: "))
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let _ = tx.send(host).await;
                let _ = s.write_all(response.as_bytes()).await;
            }
        });
        (addr, rx)
    }

    #[tokio::test]
    async fn only_the_exact_marker_with_200_is_an_answer() {
        let origin = quick_origin("https://m-1.trycloudflare.com").unwrap();
        let ok = "HTTP/1.1 200 OK\r\ncontent-length: 6\r\nconnection: close\r\n\r\nabc123";
        let (addr, mut hosts) = canned(ok).await;
        let probe = Probe::Loopback(addr);
        assert!(
            answers(probe, None, &origin, "abc123", None)
                .await
                .everywhere()
        );
        assert_eq!(hosts.recv().await.as_deref(), Some("m-1.trycloudflare.com"));
        for (response, why) in [
            (
                "HTTP/1.1 200 OK\r\ncontent-length: 6\r\nconnection: close\r\n\r\nabc124",
                "another generation's marker",
            ),
            (
                "HTTP/1.1 200 OK\r\ncontent-length: 7\r\nconnection: close\r\n\r\nabc1234",
                "a marker with more after it",
            ),
            (
                "HTTP/1.1 302 Found\r\nlocation: https://elsewhere.example/\r\ncontent-length: 6\r\nconnection: close\r\n\r\nabc123",
                "a redirect, even with the marker",
            ),
            (
                "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                "nothing armed",
            ),
        ] {
            let (addr, _) = canned(response).await;
            let a = answers(Probe::Loopback(addr), None, &origin, "abc123", None).await;
            assert!(!a.somewhere(), "{why}");
        }
    }

    #[tokio::test]
    async fn each_reachable_edge_address_is_counted() {
        let origin = quick_origin("https://m-2.trycloudflare.com").unwrap();
        let ok = "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nmk";
        let not_yet = "HTTP/1.1 530 \r\ncontent-length: 4\r\nconnection: close\r\n\r\n1016";
        let (good, _) = canned(ok).await;
        let (bad, _) = canned(not_yet).await;
        // A port nothing listens on: an address with no way in.
        let closed = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let found = |reached, answered| Answers { reached, answered };
        let a = answers_at(&[good, closed], None, &origin, "mk", None).await;
        assert_eq!(a, found(1, 1), "an address with no way in is not counted");
        assert!(a.everywhere());
        let a = answers_at(&[good, bad], None, &origin, "mk", None).await;
        assert_eq!(a, found(2, 1));
        assert!(
            !a.everywhere() && a.somewhere(),
            "one edge address not routing the name yet"
        );
        let a = answers_at(&[closed], None, &origin, "mk", None).await;
        assert!(!a.somewhere() && !a.everywhere(), "nothing reached");
        let a = answers_at(&[], None, &origin, "mk", None).await;
        assert!(!a.somewhere() && !a.everywhere());
    }

    #[test]
    fn an_overlong_log_line_is_skipped_whole() {
        let long = "x".repeat(MAX_LINE * 2);
        let log = format!("{long}\nshort\n");
        let mut r = std::io::Cursor::new(log.into_bytes());
        let mut buf = Vec::new();
        assert_eq!(
            bounded_line(&mut r, &mut buf).unwrap().as_deref(),
            Some("short\n")
        );
        assert_eq!(bounded_line(&mut r, &mut buf).unwrap(), None);
    }
}
