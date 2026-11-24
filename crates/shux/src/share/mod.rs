//! `shux share` (#297): a private, read-only view of one session in a
//! browser, through a capability link.
//!
//! The daemon owns one [`Gateway`]. It serves HTTP on a loopback port that a
//! tunnel exposes at a public HTTPS origin; it never listens elsewhere. A
//! link names a session and nothing else: each viewer streams through a
//! private read-only attach to that session, which holds no PTY writer and
//! never sizes the host. Revoke, kick, stop and session deletion close the
//! sockets of the streams they end before they report success, and say so
//! when one outlives the teardown deadline.
//!
//! [`Gateway::open`] brings up the quick tunnel ([`tunnel`]) on the first
//! link and mints it; the tunnel goes down once no guest can reach the
//! daemon any more, on [`Gateway::stop`], and when cloudflared dies, which
//! ends every link it carried. These are what the `share.*` RPCs call.

pub mod grants;
mod http;
mod stream;
pub mod tunnel;
pub mod wire;

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};
use std::time::{Duration, SystemTime};

use shux_core::event::EventData;
use shux_core::model::SessionId;
use tokio::sync::Mutex;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::attach::AttachDeps;
use grants::{Admit, Live, Now, Redeemed, Refused, Store};
pub use grants::{CreateError, CreateOpts, EndReason, GrantId, GrantInfo, Uses, ViewerId};
pub use tunnel::{Probe, TunnelError};

/// Streams open at once, across the daemon.
pub const MAX_STREAMS: usize = 16;
/// How long an operation that ends streams waits, for all of them together,
/// for their sockets to close.
const TEARDOWN: Duration = Duration::from_secs(5);
/// Failed redemptions one client may make within [`FAILURE_WINDOW`], and all
/// clients together. Past either, redemption answers 429 untried.
const CLIENT_FAILURES: usize = 10;
const ALL_FAILURES: usize = 300;
const FAILURE_WINDOW: Duration = Duration::from_secs(60);
const MAX_TRACKED_CLIENTS: usize = 4096;
/// How often a live share looks for sessions that were deleted under it.
const SESSION_CHECK: Duration = Duration::from_secs(2);
/// The most `share.*` events held for announcing; past it the oldest go.
const MAX_PENDING_EVENTS: usize = 1024;
/// What `share list` says of the tunnel, without waiting on one starting.
const TUNNEL_DOWN: u8 = 0;
const TUNNEL_STARTING: u8 = 1;
const TUNNEL_UP: u8 = 2;

/// The public origin a tunnel serves the gateway at: `https://host[:port]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    origin: String,
    host: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not an https origin: {0:?}")]
pub struct OriginError(String);

impl Origin {
    /// `https://` and a host, optionally a port; nothing after it.
    pub fn parse(s: &str) -> Result<Self, OriginError> {
        let bad = || OriginError(s.to_string());
        let host = s.strip_prefix("https://").ok_or_else(bad)?;
        let ok = !host.is_empty()
            && host.bytes().all(|b| {
                b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b':' | b'[' | b']')
            });
        if !ok {
            return Err(bad());
        }
        let host = host.to_ascii_lowercase();
        Ok(Self {
            origin: format!("https://{host}"),
            host,
        })
    }
    pub fn as_str(&self) -> &str {
        &self.origin
    }
    pub fn host(&self) -> &str {
        &self.host
    }
}

/// A link just made. `url` carries its secret and is returned only here;
/// no debug print shows it.
#[derive(Clone)]
pub struct Created {
    pub id: GrantId,
    pub url: String,
    pub expires_wall: SystemTime,
}

impl std::fmt::Debug for Created {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Created")
            .field("id", &self.id)
            .field("expires_wall", &self.expires_wall)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ShareError {
    #[error("the gateway has no public origin yet")]
    NoOrigin,
    #[error("session {0} not found")]
    NoSession(shux_core::model::SessionId),
    #[error(transparent)]
    Create(#[from] CreateError),
    #[error("no such link")]
    NoGrant,
    #[error("no such viewer")]
    NoViewer,
    #[error("the gateway's listener must be loopback, not {0}")]
    NotLoopback(SocketAddr),
    #[error("binding the gateway: {0}")]
    Bind(#[from] std::io::Error),
    /// Their credentials are gone, but these streams' sockets were still
    /// open at the deadline.
    #[error("{0} viewer stream(s) still open after {TEARDOWN:?}")]
    Teardown(usize),
    #[error(transparent)]
    Tunnel(#[from] TunnelError),
}

struct Server {
    addr: SocketAddr,
    /// A duplicate of the listener, for the tunnel's supervisor to hold.
    fd: OwnedFd,
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

/// One tunnel generation: links minted under it die with it.
struct Connector {
    generation: u64,
    supervisor: tunnel::Supervisor,
    /// Fires when cloudflared or its supervisor has gone.
    lost: CancellationToken,
}

/// Why a tunnel generation is taken down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Down {
    /// cloudflared or its supervisor went away.
    Lost,
    /// No guest can reach the daemon any more.
    Idle,
    Stopped,
}

impl Down {
    fn code(self) -> &'static str {
        match self {
            Self::Lost => "lost",
            Self::Idle => "idle",
            Self::Stopped => "stopped",
        }
    }
    /// What the links it carried end as.
    fn ends_as(self) -> EndReason {
        match self {
            Self::Lost => EndReason::TunnelLost,
            Self::Idle | Self::Stopped => EndReason::Stopped,
        }
    }
}

/// Failed redemptions, by client and in all, within [`FAILURE_WINDOW`].
#[derive(Default)]
struct Failures {
    by_client: HashMap<String, VecDeque<Instant>>,
    all: VecDeque<Instant>,
}

impl Failures {
    fn trim(q: &mut VecDeque<Instant>, now: Instant) {
        while q.front().is_some_and(|&t| t + FAILURE_WINDOW <= now) {
            q.pop_front();
        }
    }

    fn allowed(&mut self, client: &str, now: Instant) -> bool {
        Self::trim(&mut self.all, now);
        let mine = self.by_client.get_mut(client).map_or(0, |q| {
            Self::trim(q, now);
            q.len()
        });
        self.all.len() < ALL_FAILURES && mine < CLIENT_FAILURES
    }

    fn record(&mut self, client: &str, now: Instant) {
        if self.by_client.len() >= MAX_TRACKED_CLIENTS && !self.by_client.contains_key(client) {
            self.by_client.retain(|_, q| {
                Self::trim(q, now);
                !q.is_empty()
            });
            // Every one recent: the global limit still holds them all.
            if self.by_client.len() >= MAX_TRACKED_CLIENTS {
                self.by_client.clear();
            }
        }
        self.by_client
            .entry(client.to_string())
            .or_default()
            .push_back(now);
        self.all.push_back(now);
    }
}

pub(crate) struct Inner {
    deps: AttachDeps,
    store: StdMutex<Store>,
    streams: Arc<tokio::sync::Semaphore>,
    failures: StdMutex<Failures>,
    server: Mutex<Option<Server>>,
    tunnel: Mutex<Option<Connector>>,
    /// The public host a new tunnel is being probed at, and its marker.
    probe: StdMutex<Option<(String, String)>>,
    /// One per `open` in progress: `stop` cancels them all.
    starting: StdMutex<Vec<(u64, CancellationToken)>>,
    /// Links minted for a caller that has not yet said it delivered them.
    pending: StdMutex<HashSet<GrantId>>,
    /// [`TUNNEL_DOWN`], [`TUNNEL_STARTING`] or [`TUNNEL_UP`].
    tunnel_state: AtomicU8,
    /// Numbers tunnel generations and startups.
    tunnels: AtomicU64,
    /// `share.*` events in the order they happened, not yet published.
    events: StdMutex<VecDeque<EventData>>,
    /// Wakes the keeper of a live tunnel: links changed.
    changed: tokio::sync::Notify,
    /// The daemon's own shutdown.
    shutdown: CancellationToken,
    sent: AtomicU64,
    clock: fn() -> Now,
    /// Wakes streams to recheck expiry when the clock moves other than by
    /// time passing; the 10 s check catches a wall clock that jumps.
    clock_moved: tokio::sync::Notify,
}

/// The daemon's share gateway. Cheap to clone.
#[derive(Clone)]
pub struct Gateway {
    inner: Arc<Inner>,
}

/// Held by one stream from admission until its socket is gone: frees (or
/// hands its replacement) its slot, frees its credential, then fires its
/// `done`. In that order, so whoever `done` wakes finds them free.
pub(crate) struct StreamGuard {
    gw: Gateway,
    viewer: ViewerId,
    live: Live,
    slot: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        // To a replacement still waiting for it; otherwise (none, or one cut
        // off meanwhile) the slot is freed here.
        if let (Some(slot), Some(to)) = (self.slot.take(), self.live.slot_to()) {
            let _ = to.send(slot);
        }
        let now = self.gw.now().mono;
        self.gw
            .store()
            .stream_ended(&self.viewer, self.live.stream(), now);
        self.live.done().cancel();
    }
}

/// Removes its startup token from the list `stop` cancels, and cancels it,
/// however `open` ends: a caller that hangs up ends its startup.
struct Starting<'a> {
    gw: &'a Gateway,
    id: u64,
    token: CancellationToken,
}

impl Drop for Starting<'_> {
    fn drop(&mut self) {
        lock(&self.gw.inner.starting).retain(|(id, _)| *id != self.id);
        self.token.cancel();
    }
}

fn lock<T>(m: &StdMutex<T>) -> MutexGuard<'_, T> {
    // Nothing panics while holding these; a poisoned one is still whole.
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub fn rfc3339(t: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[expect(
    clippy::disallowed_methods,
    reason = "the keeper wakes at the next link's expiry; never synchronises a test"
)]
async fn until(deadline: Instant) {
    tokio::time::sleep_until(deadline).await;
}

impl Gateway {
    pub fn new(deps: AttachDeps, shutdown: CancellationToken) -> Self {
        Self::with_clock(deps, shutdown, Now::current)
    }

    fn with_clock(deps: AttachDeps, shutdown: CancellationToken, clock: fn() -> Now) -> Self {
        Self {
            inner: Arc::new(Inner {
                deps,
                store: StdMutex::default(),
                streams: Arc::new(tokio::sync::Semaphore::new(MAX_STREAMS)),
                failures: StdMutex::default(),
                server: Mutex::default(),
                tunnel: Mutex::default(),
                probe: StdMutex::default(),
                starting: StdMutex::default(),
                pending: StdMutex::default(),
                tunnel_state: AtomicU8::new(TUNNEL_DOWN),
                tunnels: AtomicU64::new(0),
                events: StdMutex::default(),
                changed: tokio::sync::Notify::new(),
                shutdown,
                sent: AtomicU64::new(0),
                clock,
                clock_moved: tokio::sync::Notify::new(),
            }),
        }
    }

    fn now(&self) -> Now {
        (self.inner.clock)()
    }

    fn store(&self) -> MutexGuard<'_, Store> {
        lock(&self.inner.store)
    }

    fn origin(&self) -> Option<(Origin, u64)> {
        self.store().origin()
    }

    /// Serves on `addr`, a loopback address (port 0 picks one). Serving
    /// already, it returns where.
    pub async fn listen(&self, addr: SocketAddr) -> Result<SocketAddr, ShareError> {
        if !addr.ip().is_loopback() {
            return Err(ShareError::NotLoopback(addr));
        }
        let mut server = self.inner.server.lock().await;
        if let Some(s) = server.as_ref().filter(|s| !s.task.is_finished()) {
            return Ok(s.addr);
        }
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let addr = listener.local_addr()?;
        let fd = listener.as_fd().try_clone_to_owned()?;
        let cancel = self.inner.shutdown.child_token();
        let task = tokio::spawn(http::serve(listener, self.clone(), cancel.clone()));
        *server = Some(Server {
            addr,
            fd,
            cancel,
            task,
        });
        Ok(addr)
    }

    /// The origin links are minted for and requests must name. A different
    /// origin ends every link made for the old one, in the same step.
    pub async fn set_origin(&self, origin: Origin) -> Result<(), ShareError> {
        let ended = self.store().set_origin(origin);
        settle(ended).await
    }

    /// Mints a link to `opts.session` at the current origin.
    pub fn create(&self, opts: CreateOpts) -> Result<Created, ShareError> {
        let now = self.now();
        let mut store = self.store();
        let (origin, _) = store.origin().ok_or(ShareError::NoOrigin)?;
        // Read under the store's lock, so no prune can miss the link.
        let sessions = self.inner.deps.graph.snapshot();
        if !sessions.sessions.contains_key(&opts.session) {
            return Err(ShareError::NoSession(opts.session));
        }
        let _ = store.end_grants(now, |s| !sessions.sessions.contains_key(&s));
        let session = opts.session;
        let minted = store.create(opts, now)?;
        drop(store);
        self.note(EventData::ShareCreated {
            link_id: minted.id.as_str().to_string(),
            session_id: session,
            expires_at: rfc3339(minted.expires_wall),
        });
        Ok(Created {
            url: format!(
                "{}/v/{}#{}",
                origin.as_str(),
                minted.id.as_str(),
                minted.secret
            ),
            id: minted.id,
            expires_wall: minted.expires_wall,
        })
    }

    fn session_exists(&self, session: SessionId) -> Result<(), ShareError> {
        self.inner
            .deps
            .graph
            .snapshot()
            .sessions
            .contains_key(&session)
            .then_some(())
            .ok_or(ShareError::NoSession(session))
    }

    /// Brings the tunnel up if it is not, then mints a link to
    /// `opts.session` through it. With `hold`, the link is revoked if that
    /// token fires before [`Self::confirm`] says the link was delivered.
    /// Dropped part way, it ends its startup, which cleans up after itself.
    pub async fn open(
        &self,
        opts: CreateOpts,
        hold: Option<CancellationToken>,
    ) -> Result<Created, ShareError> {
        opts.check()?;
        self.session_exists(opts.session)?;
        // Registered before anything is awaited, so a `stop` from here on
        // ends this startup, wherever it has got to.
        let starting = Starting {
            gw: self,
            id: self.inner.tunnels.fetch_add(1, Ordering::Relaxed),
            token: self.inner.shutdown.child_token(),
        };
        lock(&self.inner.starting).push((starting.id, starting.token.clone()));
        // On its own task, so a caller hanging up cancels the startup
        // instead of cutting its teardown short.
        let (gw, token) = (self.clone(), starting.token.clone());
        tokio::spawn(async move { gw.open_now(opts, &token, hold).await })
            .await
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "share: a startup task failed");
                Err(TunnelError::Cancelled.into())
            })
    }

    async fn open_now(
        &self,
        opts: CreateOpts,
        token: &CancellationToken,
        hold: Option<CancellationToken>,
    ) -> Result<Created, ShareError> {
        let mut tunnel = self.inner.tunnel.lock().await;
        if let Some(old) = tunnel.take_if(|c| c.lost.is_cancelled()) {
            // The links it carried end either way; the next tunnel is new.
            let _ = self.down(old, Down::Lost).await;
        }
        if tunnel.is_none() {
            let state = &self.inner.tunnel_state;
            state.store(TUNNEL_STARTING, Ordering::Relaxed);
            let up = if token.is_cancelled() {
                Err(TunnelError::Cancelled.into())
            } else {
                self.up(token).await
            };
            state.store(
                if up.is_ok() { TUNNEL_UP } else { TUNNEL_DOWN },
                Ordering::Relaxed,
            );
            match up {
                Ok(c) => *tunnel = Some(c),
                Err(e) => {
                    if let ShareError::Tunnel(t) = &e {
                        self.note(EventData::ShareTunnel {
                            state: "failed".into(),
                            reason: Some(t.code().into()),
                        });
                    }
                    drop(tunnel);
                    self.flush().await;
                    return Err(e);
                }
            }
        }
        // Under the lock `stop` takes next: a startup it cancelled mints
        // nothing, whether it brought the tunnel up or found it up.
        let created = if token.is_cancelled() {
            Err(TunnelError::Cancelled.into())
        } else {
            self.create(opts)
        };
        if let (Ok(c), Some(life)) = (&created, hold) {
            self.hold(c.id.clone(), life);
        }
        drop(tunnel);
        self.inner.changed.notify_one();
        self.flush().await;
        created
    }

    /// Revokes `id` once `life` fires, unless [`Self::confirm`] came first.
    fn hold(&self, id: GrantId, life: CancellationToken) {
        lock(&self.inner.pending).insert(id.clone());
        let gw = self.clone();
        tokio::spawn(async move {
            tokio::select! {
                () = life.cancelled() => {}
                () = gw.inner.shutdown.cancelled() => return,
            }
            if lock(&gw.inner.pending).remove(&id) {
                match gw.revoke(&id).await {
                    Ok(()) | Err(ShareError::NoGrant) => {}
                    Err(e) => tracing::warn!(error = %e, "share: revoking an undelivered link"),
                }
            }
        });
    }

    /// The caller of a held `open` has delivered link `id`: it stays.
    pub fn confirm(&self, id: &GrantId) -> Result<(), ShareError> {
        lock(&self.inner.pending)
            .remove(id)
            .then_some(())
            .ok_or(ShareError::NoGrant)
    }

    /// Starts a supervised quick tunnel to the gateway and proves it reaches
    /// this generation's marker. Failing, it stops what it started first.
    async fn up(&self, cancel: &CancellationToken) -> Result<Connector, ShareError> {
        let cloudflared = tunnel::find_cloudflared()?;
        let mut supervisor = None;
        let started = self.start(&cloudflared, &mut supervisor, cancel).await;
        if started.is_err() {
            *lock(&self.inner.probe) = None;
            if let Some(s) = supervisor {
                s.stop().await;
            }
            self.stop_serving(Instant::now() + TEARDOWN).await;
        }
        started
    }

    async fn start(
        &self,
        cloudflared: &std::path::Path,
        held: &mut Option<tunnel::Supervisor>,
        cancel: &CancellationToken,
    ) -> Result<Connector, ShareError> {
        let addr = self
            .listen(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .map_err(|_| TunnelError::SpawnFailed)?;
        let fd = {
            let server = self.inner.server.lock().await;
            let s = server.as_ref().ok_or(TunnelError::SpawnFailed)?;
            s.fd.try_clone().map_err(|_| TunnelError::SpawnFailed)?
        };
        let shux = std::env::current_exe().map_err(|_| TunnelError::SpawnFailed)?;
        let probe = default_probe(addr);
        let supervisor = held.insert(tunnel::Supervisor::spawn(
            &shux,
            cloudflared,
            &fd,
            addr.port(),
        )?);
        drop(fd);
        let origin = supervisor.origin(cancel).await?;
        let marker = tunnel::marker().map_err(|_| TunnelError::SpawnFailed)?;
        *lock(&self.inner.probe) = Some((origin.host().to_string(), marker.clone()));
        let reached = tunnel::verify(probe, &origin, &marker, cancel).await;
        *lock(&self.inner.probe) = None;
        reached?;
        // A new tunnel is a new origin: nothing minted before it reaches it.
        let ended = {
            let mut store = self.store();
            let links = store.links();
            let ended = store.end_all(EndReason::Stopped);
            drop(store);
            for (id, session) in links {
                self.note_ended(id, session, EndReason::Stopped);
            }
            ended
        };
        settle(ended).await?;
        // Published only now; `end_all` left nothing at the old origin.
        drop(self.store().set_origin(origin));
        let generation = self.inner.tunnels.fetch_add(1, Ordering::Relaxed);
        let mut supervisor = held.take().ok_or(TunnelError::SpawnFailed)?;
        let lost = supervisor.watch();
        tokio::spawn(self.clone().keep(generation, lost.clone()));
        self.note(EventData::ShareTunnel {
            state: "up".into(),
            reason: None,
        });
        Ok(Connector {
            generation,
            supervisor,
            lost,
        })
    }

    /// Takes a generation down: ends what it carried, stops serving, then
    /// stops and reaps the connector, whatever the teardown reports.
    async fn down(&self, c: Connector, why: Down) -> Result<(), ShareError> {
        let ended = {
            let mut store = self.store();
            let links = store.links();
            let ended = store.end_all(why.ends_as());
            store.clear_origin();
            drop(store);
            for (id, session) in links {
                self.note_ended(id, session, why.ends_as());
            }
            ended
        };
        let deadline = Instant::now() + TEARDOWN;
        let settled = settle_by(ended, deadline).await;
        self.stop_serving(deadline).await;
        c.supervisor.stop().await;
        self.inner
            .tunnel_state
            .store(TUNNEL_DOWN, Ordering::Relaxed);
        self.note(EventData::ShareTunnel {
            state: "down".into(),
            reason: Some(why.code().into()),
        });
        self.flush().await;
        settled
    }

    /// Keeps generation `generation` until no guest can reach it, it is
    /// lost, or another call took it down.
    async fn keep(self, generation: u64, lost: CancellationToken) {
        loop {
            let next = self
                .store()
                .next_expiry()
                .map_or(Instant::now() + SESSION_CHECK, |e| {
                    e.min(Instant::now() + SESSION_CHECK)
                });
            let lost_now = tokio::select! {
                () = self.inner.shutdown.cancelled() => return,
                () = lost.cancelled() => true,
                () = self.inner.changed.notified() => false,
                () = until(next) => false,
            };
            self.prune();
            if lost_now || self.store().reachable().is_empty() {
                // Decided again under the lock a new link is minted under.
                let mut tunnel = self.inner.tunnel.lock().await;
                self.prune();
                let idle = self.store().reachable().is_empty();
                let reason = if lost.is_cancelled() {
                    Down::Lost
                } else {
                    Down::Idle
                };
                if lost.is_cancelled() || idle {
                    if let Some(c) = tunnel.take_if(|c| c.generation == generation)
                        && let Err(e) = self.down(c, reason).await
                    {
                        tracing::warn!(error = %e, "share: taking the tunnel down");
                    }
                    return;
                }
            }
            self.flush().await;
        }
    }

    pub fn list(&self) -> Vec<GrantInfo> {
        self.prune();
        self.store().list()
    }

    /// `up`, `starting` or `down`, read without waiting on a startup.
    pub fn tunnel_state(&self) -> &'static str {
        match self.inner.tunnel_state.load(Ordering::Relaxed) {
            TUNNEL_UP => "up",
            TUNNEL_STARTING => "starting",
            _ => "down",
        }
    }

    /// Ends link `id` and every viewer it admitted; returns once their
    /// sockets are closed.
    pub async fn revoke(&self, id: &GrantId) -> Result<(), ShareError> {
        let (session, ended) = {
            let mut store = self.store();
            let session = *store.links().get(id).ok_or(ShareError::NoGrant)?;
            let at = self.now().mono;
            (session, store.revoke(id, at).ok_or(ShareError::NoGrant)?)
        };
        self.note_ended(id.clone(), session, EndReason::Revoked);
        let settled = settle(ended).await;
        self.flush().await;
        settled
    }

    /// Ends every link in one step, then waits for their viewers' sockets
    /// together: returns the links it ended and how that teardown went.
    pub async fn revoke_all(&self) -> (Vec<GrantId>, Result<(), ShareError>) {
        self.prune();
        let at = self.now().mono;
        let (revoked, ended) = {
            let mut store = self.store();
            let links = store.links();
            let ended: Vec<Live> = links
                .keys()
                .filter_map(|id| store.revoke(id, at))
                .flatten()
                .collect();
            (links, ended)
        };
        for (id, session) in &revoked {
            self.note_ended(id.clone(), *session, EndReason::Revoked);
        }
        let settled = settle(ended).await;
        self.flush().await;
        (revoked.into_keys().collect(), settled)
    }

    /// Drops one viewer and its credential; its link stays.
    pub async fn kick(&self, viewer: &ViewerId) -> Result<(), ShareError> {
        let ended = self.store().kick(viewer).ok_or(ShareError::NoViewer)?;
        self.inner.changed.notify_one();
        let settled = settle(ended.into_iter().collect()).await;
        self.flush().await;
        settled
    }

    /// Ends every link and viewer, refuses whatever was in flight under them,
    /// stops serving, and takes the tunnel down. Idempotent.
    pub async fn stop(&self) -> Result<(), ShareError> {
        // A tunnel still starting holds the lock below until it gives up.
        for (_, starting) in lock(&self.inner.starting).drain(..) {
            starting.cancel();
        }
        // Held to the end: an opener after this starts on what stop left.
        let mut tunnel = self.inner.tunnel.lock().await;
        let down = match tunnel.take() {
            Some(c) => self.down(c, Down::Stopped).await,
            None => Ok(()),
        };
        let ended = {
            let mut store = self.store();
            let links = store.links();
            let ended = store.stop();
            drop(store);
            for (id, session) in links {
                self.note_ended(id, session, EndReason::Stopped);
            }
            ended
        };
        // One deadline for the streams and the listener together.
        let deadline = Instant::now() + TEARDOWN;
        let settled = settle_by(ended, deadline).await;
        self.stop_serving(deadline).await;
        drop(tunnel);
        self.flush().await;
        down.and(settled)
    }

    async fn stop_serving(&self, deadline: Instant) {
        if let Some(server) = self.inner.server.lock().await.take() {
            server.cancel.cancel();
            if tokio::time::timeout_at(deadline, server.task)
                .await
                .is_err()
            {
                tracing::warn!("share: the listener outlived stop's deadline");
            }
        }
    }

    /// Terminal bytes written to viewers' sockets, over the gateway's life.
    pub fn bytes_sent(&self) -> u64 {
        self.inner.sent.load(Ordering::Relaxed)
    }

    /// The marker a tunnel being probed answers with at `host`.
    fn probe_marker(&self, host: &str) -> Option<String> {
        lock(&self.inner.probe)
            .as_ref()
            .filter(|(h, _)| h.eq_ignore_ascii_case(host))
            .map(|(_, m)| m.clone())
    }

    /// Ends the links to sessions that no longer exist, and expired ones,
    /// noting each, once, for the reason that ended it.
    fn prune(&self) {
        let now = self.now();
        let mut store = self.store();
        let live = self.inner.deps.graph.snapshot();
        let before = store.links();
        // Each ended stream has been told; none is waited for here.
        let _ = store.end_grants(now, |_| false);
        let swept = store.links();
        let _ = store.end_grants(now, |s| !live.sessions.contains_key(&s));
        let after = store.links();
        drop(store);
        let gone = |from: &HashMap<GrantId, SessionId>, to: &HashMap<GrantId, SessionId>| {
            from.iter()
                .filter(|(id, _)| !to.contains_key(*id))
                .map(|(id, s)| (id.clone(), *s))
                .collect::<Vec<_>>()
        };
        for (id, session) in gone(&before, &swept) {
            self.note_ended(id, session, EndReason::Expired);
        }
        for (id, session) in gone(&swept, &after) {
            self.note_ended(id, session, EndReason::SessionEnded);
        }
    }

    /// Redeems for `client`, under the generation its request was checked
    /// in, unless it has failed too often of late.
    fn redeem(
        &self,
        client: &str,
        generation: u64,
        id: &GrantId,
        secret: &str,
    ) -> Result<Redeemed, Refused> {
        let now = self.now();
        if !lock(&self.inner.failures).allowed(client, now.mono) {
            return Err(Refused::Throttled);
        }
        self.prune();
        let redeemed = {
            let mut store = self.store();
            if store.generation() == generation {
                store.redeem(id, secret, now)
            } else {
                Err(Refused::NotFound)
            }
        };
        // A wrong or unknown secret is a guess; a used-up link's right one is not.
        if let Err(Refused::NotFound) = redeemed {
            lock(&self.inner.failures).record(client, now.mono);
        }
        redeemed
    }

    /// Admits under the generation the request was checked in.
    fn admit(&self, generation: u64, id: &GrantId, cookie: &str) -> Admit {
        let now = self.now();
        let mut store = self.store();
        if store.generation() != generation {
            return Admit::Refused(Refused::NotFound);
        }
        store.admit(id, cookie, now)
    }

    /// Queues `data` for the next [`Self::flush`], in order.
    fn note(&self, data: EventData) {
        let mut events = lock(&self.inner.events);
        if events.len() >= MAX_PENDING_EVENTS {
            events.pop_front();
        }
        events.push_back(data);
        drop(events);
        self.inner.changed.notify_one();
    }

    fn note_ended(&self, id: GrantId, session: SessionId, why: EndReason) {
        self.note(EventData::ShareEnded {
            link_id: id.as_str().to_string(),
            session_id: session,
            reason: why.code().into(),
        });
    }

    /// Announces what happened, and shows hosts which of their sessions a
    /// guest can still reach.
    async fn flush(&self) {
        let events = std::mem::take(&mut *lock(&self.inner.events));
        let bus = self.inner.deps.io_state.lock().await.event_bus.clone();
        if let Some(bus) = bus {
            for e in events {
                bus.publish(e);
            }
        }
        let reachable: HashSet<SessionId> = self.store().reachable();
        let clients = self.inner.deps.io_state.lock().await.clients.clone();
        clients.set_shared(reachable);
    }
}

/// The probe a daemon uses: the public origin, unless a debug build is told
/// to fetch from its own listener (`SHUX_TEST_SHARE_PROBE=loopback`), for
/// tests whose cloudflared is a stand-in.
fn default_probe(listener: SocketAddr) -> Probe {
    #[cfg(debug_assertions)]
    if std::env::var("SHUX_TEST_SHARE_PROBE").as_deref() == Ok("loopback") {
        return Probe::Loopback(listener);
    }
    let _ = listener;
    Probe::Public
}

/// Waits, all together and within one deadline, for each ended stream to
/// close its socket.
async fn settle(ended: Vec<Live>) -> Result<(), ShareError> {
    settle_by(ended, Instant::now() + TEARDOWN).await
}

/// [`settle`], by an absolute `deadline` a caller shares with other work.
async fn settle_by(ended: Vec<Live>, deadline: Instant) -> Result<(), ShareError> {
    let all = futures::future::join_all(ended.iter().map(|l| l.done().cancelled()));
    if tokio::time::timeout_at(deadline, all).await.is_ok() {
        return Ok(());
    }
    let open = ended.iter().filter(|l| !l.done().is_cancelled()).count();
    tracing::warn!(open, "share: viewer streams outlived their cutoff");
    Err(ShareError::Teardown(open))
}

/// A gateway on an empty daemon, reading `clock`.
#[cfg(test)]
fn test_gateway(clock: fn() -> Now) -> Gateway {
    let (graph, state) = shux_core::graph::SessionGraph::new();
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let cancel = CancellationToken::new();
    tokio::spawn(shux_core::graph::run_graph_loop(graph, rx, cancel.clone()));
    let deps = AttachDeps {
        graph: shux_core::graph::GraphHandle::new(tx, state),
        io_state: Arc::new(Mutex::new(crate::pane_io::PaneIoState::new())),
        config: shux_core::config::ConfigHandle::load_or_default(std::path::Path::new(
            "/nonexistent/shux-share-tests/config.toml",
        )),
        segments: crate::statusbar_runner::SegmentCache::new(),
        meta_cache: crate::session_meta::SessionMetaCache::new(),
        onboarding: crate::onboarding::OnboardingHandle::from_state_for_test(Default::default()),
        daemon_start: std::time::Instant::now(),
    };
    Gateway::with_clock(deps, cancel, clock)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gateway() -> Gateway {
        test_gateway(Now::current)
    }

    /// A stream's slot is free by the time it says it is done: a reload at
    /// the daemon's cap, admitted the moment the old stream is done, must
    /// find its slot.
    #[tokio::test]
    async fn a_stream_frees_its_slot_before_it_says_it_is_done() {
        use std::task::{Context, Poll, Wake, Waker};
        struct Check(
            Arc<tokio::sync::Semaphore>,
            Arc<std::sync::atomic::AtomicUsize>,
        );
        impl Wake for Check {
            fn wake(self: Arc<Self>) {
                self.1.store(self.0.available_permits(), Ordering::SeqCst);
            }
        }
        let gw = gateway();
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(usize::MAX));
        let mut store = Store::default();
        let opts = CreateOpts::new(shux_core::model::SessionId::new());
        let now = grants::Now::current();
        let minted = store.create(opts, now).unwrap();
        let cookie = store
            .redeem(&minted.id, &minted.secret, now)
            .unwrap()
            .cookie;
        let grants::Admit::Ready(admitted) = store.admit(&minted.id, &cookie, now) else {
            panic!("not admitted");
        };
        let live = admitted.live;
        let guard = StreamGuard {
            gw,
            viewer: ViewerId::parse("abcdefgh").unwrap(),
            live: live.clone(),
            slot: Some(slots.clone().try_acquire_owned().unwrap()),
        };
        let done = live.done().cancelled();
        let mut done = std::pin::pin!(done);
        // The waker runs inside `cancel`, while the guard is mid-drop.
        let waker = Waker::from(Arc::new(Check(slots.clone(), seen.clone())));
        assert!(
            done.as_mut()
                .poll(&mut Context::from_waker(&waker))
                .is_pending()
        );
        drop(guard);
        assert_eq!(
            seen.load(Ordering::SeqCst),
            1,
            "done fired while the slot was still held"
        );
        assert!(matches!(
            done.poll(&mut Context::from_waker(&waker)),
            Poll::Ready(())
        ));
    }

    #[test]
    fn a_created_link_prints_without_its_secret() {
        let created = Created {
            id: GrantId::parse("abcdefgh").unwrap(),
            url: "https://x.test/v/abcdefgh#SECRETSECRET".into(),
            expires_wall: SystemTime::UNIX_EPOCH,
        };
        assert!(!format!("{created:?}").contains("SECRET"), "{created:?}");
    }

    /// A stream ended for its replacement keeps its slot for that
    /// replacement: at the daemon's cap, nobody else can take it meanwhile.
    #[tokio::test]
    async fn a_replaced_stream_hands_its_slot_to_its_replacement() {
        let gw = gateway();
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let mut store = Store::default();
        let now = grants::Now::current();
        let minted = store
            .create(CreateOpts::new(shux_core::model::SessionId::new()), now)
            .unwrap();
        let cookie = store
            .redeem(&minted.id, &minted.secret, now)
            .unwrap()
            .cookie;
        let grants::Admit::Ready(admitted) = store.admit(&minted.id, &cookie, now) else {
            panic!("not admitted");
        };
        let (to, mut from) = tokio::sync::oneshot::channel();
        admitted.live.replace(to);
        drop(StreamGuard {
            gw: gw.clone(),
            viewer: admitted.viewer.clone(),
            live: admitted.live.clone(),
            slot: Some(slots.clone().try_acquire_owned().unwrap()),
        });
        assert!(
            slots.clone().try_acquire_owned().is_err(),
            "the slot went to anyone"
        );
        assert!(from.try_recv().is_ok(), "the replacement never got it");
    }

    /// A stream ended for a replacement that never comes (its request was
    /// cut off while it waited) gives its slot back: nothing holds it.
    #[tokio::test]
    async fn a_slot_kept_for_a_replacement_that_never_comes_is_freed() {
        let gw = gateway();
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let mut store = Store::default();
        let now = grants::Now::current();
        let minted = store
            .create(CreateOpts::new(shux_core::model::SessionId::new()), now)
            .unwrap();
        let cookie = store
            .redeem(&minted.id, &minted.secret, now)
            .unwrap()
            .cookie;
        let grants::Admit::Ready(admitted) = store.admit(&minted.id, &cookie, now) else {
            panic!("not admitted");
        };
        admitted.live.end(EndReason::Replaced);
        drop(StreamGuard {
            gw: gw.clone(),
            viewer: admitted.viewer.clone(),
            live: admitted.live.clone(),
            slot: Some(slots.clone().try_acquire_owned().unwrap()),
        });
        assert_eq!(slots.available_permits(), 1, "the slot outlived its stream");
        // Nor when the replacement asked for it, then was cut off waiting.
        store.stream_ended(&admitted.viewer, admitted.live.stream(), now.mono);
        let grants::Admit::Ready(again) = store.admit(&minted.id, &cookie, now) else {
            panic!("not admitted");
        };
        let (to, from) = tokio::sync::oneshot::channel();
        again.live.replace(to);
        drop(from);
        drop(StreamGuard {
            gw: gw.clone(),
            viewer: again.viewer.clone(),
            live: again.live.clone(),
            slot: Some(slots.clone().try_acquire_owned().unwrap()),
        });
        assert_eq!(
            slots.available_permits(),
            1,
            "the slot outlived its replacement"
        );
        drop(gw);
    }

    /// A stop that finds a stream which never closes and a listener which
    /// never stops still ends at one deadline, not one each.
    #[tokio::test(start_paused = true)]
    async fn stop_keeps_to_one_deadline_for_streams_and_listener() {
        let gw = gateway();
        let now = gw.now();
        let stuck = {
            let mut store = gw.store();
            let minted = store
                .create(CreateOpts::new(shux_core::model::SessionId::new()), now)
                .unwrap();
            let cookie = store
                .redeem(&minted.id, &minted.secret, now)
                .unwrap()
                .cookie;
            let grants::Admit::Ready(stuck) = store.admit(&minted.id, &cookie, now) else {
                panic!("not admitted");
            };
            stuck
        };
        *gw.inner.server.lock().await = Some(Server {
            addr: "127.0.0.1:1".parse().unwrap(),
            fd: std::fs::File::open("/dev/null").unwrap().into(),
            cancel: CancellationToken::new(),
            task: tokio::spawn(std::future::pending()),
        });
        let started = Instant::now();
        assert!(matches!(gw.stop().await, Err(ShareError::Teardown(1))));
        assert_eq!(started.elapsed(), TEARDOWN);
        drop(stuck);
    }

    /// A request checked before a stop is admitted to nothing after it.
    #[tokio::test]
    async fn a_request_checked_before_a_stop_redeems_nothing() {
        let gw = gateway();
        let session = gw
            .inner
            .deps
            .graph
            .create_session("s".into(), std::env::temp_dir())
            .await
            .unwrap();
        gw.set_origin(Origin::parse("https://a.test").unwrap())
            .await
            .unwrap();
        let created = gw.create(CreateOpts::new(session)).unwrap();
        let secret = created.url.split_once('#').unwrap().1;
        let (_, generation) = gw.origin().unwrap();
        gw.stop().await.unwrap();
        let again = gw.create(CreateOpts::new(session)).unwrap();
        let fresh = again.url.split_once('#').unwrap().1;
        assert_eq!(
            gw.redeem("c", generation, &created.id, secret).unwrap_err(),
            Refused::NotFound
        );
        assert_eq!(
            gw.redeem("c", generation, &again.id, fresh).unwrap_err(),
            Refused::NotFound
        );
        let (_, current) = gw.origin().unwrap();
        assert!(gw.redeem("c", current, &again.id, fresh).is_ok());
    }

    /// A stream whose socket never closes is reported, after one deadline
    /// for every stream together.
    #[tokio::test(start_paused = true)]
    async fn teardown_reports_a_stream_that_outlived_it() {
        let mut store = Store::default();
        let now = grants::Now::current();
        let minted = store
            .create(CreateOpts::new(shux_core::model::SessionId::new()), now)
            .unwrap();
        let cookie = store
            .redeem(&minted.id, &minted.secret, now)
            .unwrap()
            .cookie;
        let grants::Admit::Ready(stuck) = store.admit(&minted.id, &cookie, now) else {
            panic!("not admitted");
        };
        let ended = store.revoke(&minted.id, now.mono).unwrap();
        assert!(matches!(settle(ended).await, Err(ShareError::Teardown(1))));
        stuck.live.done().cancel();
        assert!(settle(vec![stuck.live]).await.is_ok());
    }

    /// A session with an origin to mint for.
    async fn shared(gw: &Gateway) -> SessionId {
        let session = gw
            .inner
            .deps
            .graph
            .create_session("s".into(), std::env::temp_dir())
            .await
            .unwrap();
        gw.set_origin(Origin::parse("https://a.test").unwrap())
            .await
            .unwrap();
        session
    }

    /// A link with one viewer whose socket never closes.
    fn stuck(gw: &Gateway, session: SessionId) -> Live {
        let created = gw.create(CreateOpts::new(session)).unwrap();
        let secret = created.url.split_once('#').unwrap().1;
        let (_, generation) = gw.origin().unwrap();
        let cookie = gw
            .redeem("c", generation, &created.id, secret)
            .unwrap()
            .cookie;
        let Admit::Ready(admitted) = gw.admit(generation, &created.id, &cookie) else {
            panic!("not admitted");
        };
        admitted.live
    }

    /// One link's teardown failing leaves no other link valid.
    #[tokio::test(start_paused = true)]
    async fn revoking_all_ends_every_link_though_a_teardown_fails() {
        let gw = gateway();
        let session = shared(&gw).await;
        let held = [stuck(&gw, session), stuck(&gw, session)];
        let (revoked, settled) = gw.revoke_all().await;
        assert!(gw.list().is_empty(), "a link outlived revoke all");
        assert_eq!(revoked.len(), 2);
        assert!(
            matches!(settled, Err(ShareError::Teardown(2))),
            "{settled:?}"
        );
        drop(held);
    }

    /// Revoke all leaves each link's tombstone, as one revoke does: it
    /// answers "ended", whatever the secret, and costs no guess.
    #[tokio::test]
    async fn revoking_all_leaves_each_link_saying_it_ended() {
        let gw = gateway();
        let session = shared(&gw).await;
        let created = gw.create(CreateOpts::new(session)).unwrap();
        let secret = created.url.split_once('#').unwrap().1.to_string();
        gw.revoke_all().await.1.unwrap();
        let (_, generation) = gw.origin().unwrap();
        for s in [secret.as_str(), "wrong"]
            .iter()
            .cycle()
            .take(CLIENT_FAILURES + 2)
        {
            assert_eq!(
                gw.redeem("c", generation, &created.id, s).unwrap_err(),
                Refused::Gone(Some(EndReason::Revoked))
            );
        }
        let fresh = gw.create(CreateOpts::new(session)).unwrap();
        let fresh_secret = fresh.url.split_once('#').unwrap().1;
        assert!(
            gw.redeem("c", generation, &fresh.id, fresh_secret).is_ok(),
            "asking after an ended link counted as guessing"
        );
    }

    /// A startup `stop` cancelled mints nothing, even through a tunnel
    /// already up, whichever of the two gets the tunnel first.
    #[tokio::test]
    async fn a_startup_cancelled_by_stop_mints_nothing() {
        let gw = gateway();
        let session = shared(&gw).await;
        let mut tunnel = gw.inner.tunnel.lock().await;
        *tunnel = Some(Connector {
            generation: 0,
            supervisor: tunnel::Supervisor::idle(),
            lost: CancellationToken::new(),
        });
        let opening = tokio::spawn({
            let gw = gw.clone();
            async move { gw.open(CreateOpts::new(session), None).await }
        });
        shux_testkit::wait_until_async("the startup to register", || {
            let n = lock(&gw.inner.starting).len();
            async move { n == 1 }
        })
        .await;
        let stopping = tokio::spawn({
            let gw = gw.clone();
            async move { gw.stop().await }
        });
        shux_testkit::wait_until_async("stop to cancel the startup", || {
            let n = lock(&gw.inner.starting).len();
            async move { n == 0 }
        })
        .await;
        drop(tunnel);
        let opened = opening.await.unwrap();
        stopping.await.unwrap().unwrap();
        assert!(opened.is_err(), "a cancelled startup minted {opened:?}");
        assert!(gw.list().is_empty());
    }

    /// An opener that comes in while `stop` tears down waits for it, so the
    /// listener it finds is not then stopped under it.
    #[tokio::test(start_paused = true)]
    async fn an_opener_during_stop_keeps_the_listener_it_finds() {
        let gw = gateway();
        let session = shared(&gw).await;
        gw.listen(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let held = stuck(&gw, session);
        let stopping = tokio::spawn({
            let gw = gw.clone();
            async move { gw.stop().await }
        });
        shux_testkit::wait_until_async("stop to end the links", || {
            let empty = gw.list().is_empty();
            async move { empty }
        })
        .await;
        // What `open` does: the tunnel, then a listener for it.
        let tunnel = gw.inner.tunnel.lock().await;
        gw.listen(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        drop(tunnel);
        let _ = stopping.await.unwrap();
        let server = gw.inner.server.lock().await;
        assert!(
            server.as_ref().is_some_and(|s| !s.task.is_finished()),
            "stop stopped the listener an opener had found"
        );
        drop(held);
    }
}
