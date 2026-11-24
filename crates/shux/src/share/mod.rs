//! `shux share` (#297): a private, read-only view of one session in a
//! browser, through a capability link.
//!
//! The daemon owns one [`Gateway`]. It serves HTTP on a loopback port that a
//! tunnel exposes at a public HTTPS origin; it never listens elsewhere. A
//! link names a session and nothing else: each viewer streams through a
//! private read-only attach to that session, which holds no PTY writer and
//! never sizes the host. Revoke, expiry, stop and session deletion end the
//! live streams they cover before they return.
//!
//! The owner-facing operations ([`Gateway::create`], [`Gateway::list`],
//! [`Gateway::revoke`], [`Gateway::kick`], [`Gateway::stop`]) are what the
//! `share.*` RPCs call.

pub mod grants;
mod http;
mod stream;
pub mod wire;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};
use std::time::{Duration, SystemTime};

use tokio::sync::Mutex;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::attach::AttachDeps;
pub use grants::{CreateError, CreateOpts, EndReason, GrantId, GrantInfo, Uses, ViewerId};
use grants::{Live, Store};

/// Streams open at once, across the daemon.
pub const MAX_STREAMS: usize = 16;
/// How long an operation that ends streams waits for each to drop its socket.
const TEARDOWN: Duration = Duration::from_secs(5);

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

/// A link just made. `url` carries its secret and is returned only here.
#[derive(Debug, Clone)]
pub struct Created {
    pub id: GrantId,
    pub url: String,
    pub expires_wall: SystemTime,
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
}

struct Server {
    addr: SocketAddr,
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

pub(crate) struct Inner {
    deps: AttachDeps,
    store: StdMutex<Store>,
    origin: StdMutex<Option<Origin>>,
    streams: Arc<tokio::sync::Semaphore>,
    server: Mutex<Option<Server>>,
    /// The daemon's own shutdown.
    shutdown: CancellationToken,
    sent: AtomicU64,
}

impl Inner {
    fn count_sent(&self, n: u64) {
        self.sent.fetch_add(n, Ordering::Relaxed);
    }
}

/// The daemon's share gateway. Cheap to clone.
#[derive(Clone)]
pub struct Gateway {
    inner: Arc<Inner>,
}

/// Held by one stream from admission until its socket is gone: frees its
/// credential and fires its `done`, even when the upgrade never happened.
pub(crate) struct StreamGuard {
    gw: Gateway,
    viewer: ViewerId,
    live: Live,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.gw
            .store()
            .stream_ended(&self.viewer, self.live.stream());
        self.live.done().cancel();
    }
}

fn rfc3339(t: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

impl Gateway {
    pub fn new(deps: AttachDeps, shutdown: CancellationToken) -> Self {
        Self {
            inner: Arc::new(Inner {
                deps,
                store: StdMutex::default(),
                origin: StdMutex::default(),
                streams: Arc::new(tokio::sync::Semaphore::new(MAX_STREAMS)),
                server: Mutex::default(),
                shutdown,
                sent: AtomicU64::new(0),
            }),
        }
    }

    fn store(&self) -> MutexGuard<'_, Store> {
        // Nothing panics while holding it; a poisoned store is still whole.
        self.inner
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn origin(&self) -> Option<Origin> {
        self.inner
            .origin
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
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
    /// origin ends every link made for the old one.
    pub async fn set_origin(&self, origin: Origin) {
        let changed = {
            let mut held = self
                .inner
                .origin
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let changed = held.as_ref().is_some_and(|o| *o != origin);
            *held = Some(origin);
            changed
        };
        if changed {
            let ended = self.store().stop();
            settle(ended).await;
        }
    }

    /// Mints a link to `opts.session`.
    pub fn create(&self, opts: CreateOpts) -> Result<Created, ShareError> {
        let origin = self.origin().ok_or(ShareError::NoOrigin)?;
        if !self
            .inner
            .deps
            .graph
            .snapshot()
            .sessions
            .contains_key(&opts.session)
        {
            return Err(ShareError::NoSession(opts.session));
        }
        self.prune();
        let minted = self
            .store()
            .create(opts, Instant::now(), SystemTime::now())?;
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
    /// sockets are dropped.
    pub async fn revoke(&self, id: &GrantId) -> Result<(), ShareError> {
        let ended = self.store().revoke(id).ok_or(ShareError::NoGrant)?;
        settle(ended).await;
        Ok(())
    }

    /// Drops one viewer and its credential; its link stays.
    pub async fn kick(&self, viewer: &ViewerId) -> Result<(), ShareError> {
        let ended = self.store().kick(viewer).ok_or(ShareError::NoViewer)?;
        settle(ended.into_iter().collect()).await;
        Ok(())
    }

    /// Ends every link and viewer and stops serving. Idempotent.
    pub async fn stop(&self) {
        let ended = self.store().stop();
        settle(ended).await;
        if let Some(server) = self.inner.server.lock().await.take() {
            server.cancel.cancel();
            let _ = tokio::time::timeout(TEARDOWN, server.task).await;
        }
    }

    /// Terminal bytes written to viewers' sockets, over the gateway's life.
    pub fn bytes_sent(&self) -> u64 {
        self.inner.sent.load(Ordering::Relaxed)
    }

    /// Ends the links to sessions that no longer exist, and expired ones.
    fn prune(&self) {
        let live = self.inner.deps.graph.snapshot();
        let mut store = self.store();
        // Each ended stream has been told; none is waited for here.
        let _ = store.sweep(Instant::now());
        for s in store.sessions() {
            if !live.sessions.contains_key(&s) {
                let _ = store.end_session(s);
            }
        }
    }

    fn sweep(&self) {
        let _ = self.store().sweep(Instant::now());
    }
}

/// Waits for each ended stream to drop its socket.
async fn settle(ended: Vec<Live>) {
    for live in ended {
        if tokio::time::timeout(TEARDOWN, live.done().cancelled())
            .await
            .is_err()
        {
            tracing::warn!("share: a viewer stream outlived its cutoff");
        }
    }
}
