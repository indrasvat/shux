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
//! The owner-facing operations ([`Gateway::create`], [`Gateway::list`],
//! [`Gateway::revoke`], [`Gateway::kick`], [`Gateway::stop`]) are what the
//! `share.*` RPCs call.

pub mod grants;
mod http;
mod stream;
pub mod wire;

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};
use std::time::{Duration, SystemTime};

use tokio::sync::Mutex;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::attach::AttachDeps;
use grants::{Admit, Live, Now, Redeemed, Refused, Store};
pub use grants::{CreateError, CreateOpts, EndReason, GrantId, GrantInfo, Uses, ViewerId};

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
}

struct Server {
    addr: SocketAddr,
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
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

fn lock<T>(m: &StdMutex<T>) -> MutexGuard<'_, T> {
    // Nothing panics while holding these; a poisoned one is still whole.
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn rfc3339(t: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
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
        let cancel = self.inner.shutdown.child_token();
        let task = tokio::spawn(http::serve(listener, self.clone(), cancel.clone()));
        *server = Some(Server { addr, cancel, task });
        Ok(addr)
    }

    /// The origin links are minted for and requests must name. A different
    /// origin ends every link made for the old one, in the same step.
    pub async fn set_origin(&self, origin: Origin) -> Result<(), ShareError> {
        let ended = self.store().set_origin(origin);
        settle(ended).await
    }

    /// Mints a link to `opts.session`.
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
        let minted = store.create(opts, now)?;
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

    pub fn list(&self) -> Vec<GrantInfo> {
        self.prune();
        self.store().list()
    }

    /// Ends link `id` and every viewer it admitted; returns once their
    /// sockets are closed.
    pub async fn revoke(&self, id: &GrantId) -> Result<(), ShareError> {
        let ended = self.store().revoke(id).ok_or(ShareError::NoGrant)?;
        settle(ended).await
    }

    /// Drops one viewer and its credential; its link stays.
    pub async fn kick(&self, viewer: &ViewerId) -> Result<(), ShareError> {
        let ended = self.store().kick(viewer).ok_or(ShareError::NoViewer)?;
        settle(ended.into_iter().collect()).await
    }

    /// Ends every link and viewer, refuses whatever was in flight under them,
    /// and stops serving. Idempotent.
    pub async fn stop(&self) -> Result<(), ShareError> {
        // One deadline for the streams and the listener together.
        let deadline = Instant::now() + TEARDOWN;
        let ended = self.store().stop();
        let settled = settle_by(ended, deadline).await;
        if let Some(server) = self.inner.server.lock().await.take() {
            server.cancel.cancel();
            if tokio::time::timeout_at(deadline, server.task)
                .await
                .is_err()
            {
                tracing::warn!("share: the listener outlived stop's deadline");
            }
        }
        settled
    }

    /// Terminal bytes written to viewers' sockets, over the gateway's life.
    pub fn bytes_sent(&self) -> u64 {
        self.inner.sent.load(Ordering::Relaxed)
    }

    /// Ends the links to sessions that no longer exist, and expired ones.
    fn prune(&self) {
        let now = self.now();
        let mut store = self.store();
        let live = self.inner.deps.graph.snapshot();
        // Each ended stream has been told; none is waited for here.
        let _ = store.end_grants(now, |s| !live.sessions.contains_key(&s));
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
        let ended = store.revoke(&minted.id).unwrap();
        assert!(matches!(settle(ended).await, Err(ShareError::Teardown(1))));
        stuck.live.done().cancel();
        assert!(settle(vec![stuck.live]).await.is_ok());
    }
}
