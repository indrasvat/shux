//! Share links and the viewers they admitted: the gateway's whole state.
//!
//! A link is `https://<origin>/v/<id>#<secret>`. Only the SHA-256 of a secret
//! is kept, and a presented one is compared in constant time. Redeeming a
//! link spends one of its uses and mints a viewer credential of its own (a
//! cookie), so one viewer can be dropped without ending the link. Every
//! operation takes the clock as an argument: the store decides nothing by
//! reading time itself.
//!
//! No link takes the daemon: each holds at most [`MAX_VIEWERS_PER_LINK`]
//! credentials and [`MAX_STREAMS_PER_LINK`] streams, a credential offline for
//! [`OFFLINE_GRACE`] gives its place back, and one credential reconnects at
//! most [`ADMIT_BURST`] times in [`ADMIT_WINDOW`].

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest as _, Sha256};
use shux_core::model::SessionId;
use subtle::ConstantTimeEq as _;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// A link lives an hour unless asked otherwise, and never past a day.
pub const DEFAULT_TTL: Duration = Duration::from_secs(60 * 60);
pub const MAX_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Redeemable links held at once, across the daemon.
pub const MAX_GRANTS: usize = 64;
/// Viewer credentials held at once, across the daemon.
pub const MAX_VIEWERS: usize = 256;
/// Credentials one link may hold: an eighth of the daemon's.
pub const MAX_VIEWERS_PER_LINK: usize = 32;
/// Streams one link may have open: a quarter of the daemon's.
pub const MAX_STREAMS_PER_LINK: usize = 4;
/// A credential with no stream for this long is dropped, and its place freed.
pub const OFFLINE_GRACE: Duration = Duration::from_secs(10 * 60);
/// Streams one credential may open within [`ADMIT_WINDOW`].
pub const ADMIT_BURST: usize = 5;
pub const ADMIT_WINDOW: Duration = Duration::from_secs(10);
pub const MAX_LABEL: usize = 64;
/// How long, and how many, ended links are remembered by their reason.
pub const ENDED_GRACE: Duration = Duration::from_secs(60 * 60);
pub const MAX_ENDED: usize = 256;

const ID_ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
const ID_LEN: usize = 8;
const SECRET_BYTES: usize = 32;

/// 40 random bits as eight base32 characters: a lookup and log handle,
/// public by design.
fn fresh_id() -> Result<String, getrandom::Error> {
    let mut raw = [0u8; 5];
    getrandom::fill(&mut raw)?;
    let bits = raw.iter().fold(0u64, |acc, &b| (acc << 8) | u64::from(b));
    Ok((0..ID_LEN)
        .map(|i| char::from(ID_ALPHABET[((bits >> (35 - 5 * i)) & 31) as usize]))
        .collect())
}

fn valid_id(s: &str) -> bool {
    s.len() == ID_LEN && s.bytes().all(|b| ID_ALPHABET.contains(&b))
}

fn digest(secret: &[u8]) -> [u8; 32] {
    Sha256::digest(secret).into()
}

/// A fresh 256-bit secret, as the URL-safe text that carries it.
fn fresh_secret() -> Result<(String, [u8; 32]), getrandom::Error> {
    let mut raw = [0u8; SECRET_BYTES];
    getrandom::fill(&mut raw)?;
    Ok((URL_SAFE_NO_PAD.encode(raw), digest(&raw)))
}

/// The digest of a presented secret, or `None` when it is not one.
fn presented(text: &str) -> Option<[u8; 32]> {
    let raw = URL_SAFE_NO_PAD.decode(text).ok()?;
    (raw.len() == SECRET_BYTES).then(|| digest(&raw))
}

fn same(a: &[u8; 32], b: &[u8; 32]) -> bool {
    bool::from(a.ct_eq(b))
}

/// A moment on both clocks. The monotonic one stops while a laptop sleeps
/// and the wall one does not, so an expiry passes on whichever reaches it.
#[derive(Debug, Clone, Copy)]
pub struct Now {
    pub mono: Instant,
    pub wall: SystemTime,
}

impl Now {
    pub fn current() -> Self {
        Self {
            mono: Instant::now(),
            wall: SystemTime::now(),
        }
    }
}

/// When a link ends, on both clocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Expiry {
    pub mono: Instant,
    pub wall: SystemTime,
}

impl Expiry {
    pub fn passed(&self, at: Now) -> bool {
        at.mono >= self.mono || at.wall >= self.wall
    }

    /// Time left by whichever clock is nearer its end.
    pub fn remaining(&self, at: Now) -> Duration {
        self.mono
            .saturating_duration_since(at.mono)
            .min(self.wall.duration_since(at.wall).unwrap_or_default())
    }

    pub fn remaining_ms(&self, at: Now) -> u64 {
        self.remaining(at)
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    /// Whole seconds, rounded up: a cookie with under a second left is
    /// still a live one, and `Max-Age=0` would delete it.
    pub fn cookie_max_age(&self, at: Now) -> u64 {
        let left = self.remaining(at);
        left.as_secs() + u64::from(left.subsec_nanos() > 0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GrantId(String);

impl GrantId {
    pub fn parse(s: &str) -> Option<Self> {
        valid_id(s).then(|| Self(s.to_string()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// The cookie this link's viewers carry. One per link, so several links
    /// open on one origin do not overwrite each other.
    pub fn cookie_name(&self) -> String {
        format!("__Host-shux-v-{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ViewerId(String);

impl ViewerId {
    pub fn parse(s: &str) -> Option<Self> {
        valid_id(s).then(|| Self(s.to_string()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// How many times a link may be redeemed.
/// A format character (Unicode `Cf`: zero-width, direction marks and
/// overrides, BOM, tags): invisible, so a label holding one can read as
/// something it is not.
fn invisible(c: char) -> bool {
    matches!(c,
        '\u{ad}' | '\u{600}'..='\u{605}' | '\u{61c}' | '\u{6dd}' | '\u{70f}' | '\u{180e}'
        | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}'
        | '\u{2066}'..='\u{206f}' | '\u{feff}' | '\u{fff9}'..='\u{fffb}' | '\u{110bd}'
        | '\u{1d173}'..='\u{1d17a}' | '\u{e0001}' | '\u{e0020}'..='\u{e007f}')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Uses {
    Limited(std::num::NonZeroU32),
    Unlimited,
}

/// A link to mint: one session, read-only, for `ttl`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateOpts {
    pub session: SessionId,
    pub ttl: Duration,
    pub uses: Uses,
    pub label: Option<String>,
}

impl CreateOpts {
    /// One use, for an hour.
    pub fn new(session: SessionId) -> Self {
        Self {
            session,
            ttl: DEFAULT_TTL,
            uses: Uses::Limited(std::num::NonZeroU32::MIN),
            label: None,
        }
    }

    /// The checks [`Store::create`] makes of the options themselves.
    pub fn check(&self) -> Result<(), CreateError> {
        if self.ttl.is_zero() || self.ttl > MAX_TTL {
            return Err(CreateError::Ttl);
        }
        if let Some(label) = &self.label
            && (label.chars().count() > MAX_LABEL
                || label.chars().any(|c| c.is_control() || invisible(c)))
        {
            return Err(CreateError::Label);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CreateError {
    #[error("a link lasts between 1 second and {}h", MAX_TTL.as_secs() / 3600)]
    Ttl,
    #[error("a label is at most {MAX_LABEL} printable characters")]
    Label,
    #[error("{MAX_GRANTS} links are already live; revoke one first")]
    Full,
    #[error("the OS random source failed: {0}")]
    Random(getrandom::Error),
}

/// Why a viewer's stream ended, as a stable code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    Revoked,
    Expired,
    Stopped,
    SessionEnded,
    Kicked,
    /// It fell too far behind; it reconnects to a full redraw.
    Lagging,
    /// The same credential opened another stream.
    Replaced,
    /// The public tunnel went away; a new one needs new links.
    TunnelLost,
}

impl EndReason {
    pub fn code(self) -> &'static str {
        match self {
            Self::Revoked => "revoked",
            Self::Expired => "expired",
            Self::Stopped => "stopped",
            Self::SessionEnded => "session_ended",
            Self::Kicked => "kicked",
            Self::Lagging => "lagging",
            Self::Replaced => "replaced",
            Self::TunnelLost => "tunnel_lost",
        }
    }
}

/// Where a replaced stream's slot goes: to the request replacing it, while
/// that request still waits for it.
pub type SlotTo = tokio::sync::oneshot::Sender<tokio::sync::OwnedSemaphorePermit>;

/// One live stream of one viewer: ended through `stop`, and `done` once its
/// socket is gone.
#[derive(Debug, Clone)]
pub struct Live {
    stream: u64,
    stop: CancellationToken,
    reason: Arc<OnceLock<EndReason>>,
    done: CancellationToken,
    slot_to: Arc<std::sync::Mutex<Option<SlotTo>>>,
    /// Its attach is open and drawing: it counts as watching.
    drawing: Arc<std::sync::atomic::AtomicBool>,
}

impl Live {
    fn new(stream: u64) -> Self {
        Self {
            stream,
            stop: CancellationToken::new(),
            reason: Arc::default(),
            done: CancellationToken::new(),
            slot_to: Arc::default(),
            drawing: Arc::default(),
        }
    }
    /// Ends the stream for its replacement, which takes its slot through
    /// `to` if it is still waiting by then; if not, the slot is freed.
    pub fn replace(&self, to: SlotTo) {
        *self
            .slot_to
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(to);
        self.end(EndReason::Replaced);
    }
    /// The replacement waiting for this stream's slot, if any.
    pub fn slot_to(&self) -> Option<SlotTo> {
        self.slot_to
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
    /// Set by the stream once its attach is open, as the host's badge
    /// counts it.
    pub fn mark_drawing(&self) {
        self.drawing
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
    pub fn drawing(&self) -> bool {
        self.drawing.load(std::sync::atomic::Ordering::Relaxed)
    }
    /// Ends the stream for `why`; the first reason given stands.
    pub fn end(&self, why: EndReason) {
        let _ = self.reason.set(why);
        self.stop.cancel();
    }
    pub fn stop(&self) -> &CancellationToken {
        &self.stop
    }
    pub fn reason(&self) -> Option<EndReason> {
        self.reason.get().copied()
    }
    /// Fired by the stream once it has dropped its socket.
    pub fn done(&self) -> &CancellationToken {
        &self.done
    }
    pub fn stream(&self) -> u64 {
        self.stream
    }
}

#[derive(Debug)]
struct Grant {
    session: SessionId,
    digest: [u8; 32],
    expires: Expiry,
    /// `None` is unlimited.
    uses_left: Option<u32>,
    redeemed: u32,
    label: Option<String>,
}

#[derive(Debug)]
struct Viewer {
    grant: GrantId,
    digest: [u8; 32],
    live: Option<Live>,
    /// Since when it has had no stream; `None` while it streams.
    offline_since: Option<Instant>,
    /// When its recent streams were admitted, within [`ADMIT_WINDOW`].
    admitted: VecDeque<Instant>,
}

/// A link just minted. `secret` exists only here, and no debug print shows it.
#[derive(Clone)]
pub struct Minted {
    pub id: GrantId,
    pub secret: String,
    pub expires_wall: SystemTime,
}

impl std::fmt::Debug for Minted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Minted")
            .field("id", &self.id)
            .field("expires_wall", &self.expires_wall)
            .finish_non_exhaustive()
    }
}

/// A redemption that succeeded: the cookie value to set, once, and never
/// printed.
#[derive(Clone)]
pub struct Redeemed {
    pub viewer: ViewerId,
    pub cookie: String,
    pub expires: Expiry,
}

impl std::fmt::Debug for Redeemed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Redeemed")
            .field("viewer", &self.viewer)
            .field("expires", &self.expires)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// No such link, or not its secret: nothing is said about which.
    NotFound,
    /// A link that has no uses left (`None`), or that ended (and why). An
    /// ended link answers so for [`ENDED_GRACE`] whatever secret is
    /// presented: it holds none by then.
    Gone(Option<EndReason>),
    /// The daemon or the link holds as many viewers or streams as it will,
    /// or a viewer cannot be minted.
    Full,
    /// The credential reconnected too often of late.
    Throttled,
}

/// A credential admitted to stream.
#[derive(Debug, Clone)]
pub struct Admitted {
    pub viewer: ViewerId,
    pub session: SessionId,
    pub expires: Expiry,
    pub live: Live,
}

#[derive(Debug, Clone)]
pub enum Admit {
    Ready(Admitted),
    /// The credential is streaming already: end that one and wait for it.
    Busy(Live),
    Refused(Refused),
}

/// What `list` shows of one link. No secret, digest or cookie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantInfo {
    pub id: GrantId,
    pub session: SessionId,
    pub label: Option<String>,
    pub expires_wall: SystemTime,
    pub expires: Expiry,
    pub uses_left: Option<u32>,
    pub redeemed: u32,
    pub viewers: Vec<ViewerInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewerInfo {
    pub id: ViewerId,
    pub connected: bool,
}

#[derive(Debug, Default)]
pub struct Store {
    grants: HashMap<GrantId, Grant>,
    viewers: HashMap<ViewerId, Viewer>,
    next_stream: u64,
    origin: Option<super::Origin>,
    /// Links that ended of late, and why: so a guest who opens one is told.
    ended: HashMap<GrantId, (EndReason, Instant)>,
    /// Moves on with every stop and origin change: a request checked under
    /// an older one is admitted to nothing.
    generation: u64,
}

impl Store {
    /// The origin requests must name, and the generation they are checked
    /// under.
    pub fn origin(&self) -> Option<(super::Origin, u64)> {
        Some((self.origin.clone()?, self.generation))
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// A different origin ends every link made for the old one, in the same
    /// step that names the new one.
    pub fn set_origin(&mut self, origin: super::Origin) -> Vec<Live> {
        let ended = match &self.origin {
            Some(old) if *old != origin => self.stop(),
            _ => Vec::new(),
        };
        self.origin = Some(origin);
        ended
    }

    /// No origin any more: a tunnel went down. Requests checked before it
    /// are admitted to nothing.
    pub fn clear_origin(&mut self) {
        self.origin = None;
        self.generation += 1;
    }

    pub fn create(&mut self, opts: CreateOpts, at: Now) -> Result<Minted, CreateError> {
        opts.check()?;
        let _ = self.end_grants(at, |_| false);
        if self.grants.len() >= MAX_GRANTS {
            return Err(CreateError::Full);
        }
        let id = loop {
            let id = GrantId(fresh_id().map_err(CreateError::Random)?);
            if !self.grants.contains_key(&id) {
                break id;
            }
        };
        let (secret, digest) = fresh_secret().map_err(CreateError::Random)?;
        let expires = Expiry {
            mono: at.mono + opts.ttl,
            wall: at.wall + opts.ttl,
        };
        self.grants.insert(
            id.clone(),
            Grant {
                session: opts.session,
                digest,
                expires,
                uses_left: match opts.uses {
                    Uses::Limited(n) => Some(n.get()),
                    Uses::Unlimited => None,
                },
                redeemed: 0,
                label: opts.label,
            },
        );
        Ok(Minted {
            id,
            secret,
            expires_wall: expires.wall,
        })
    }

    /// Whether `secret` would redeem `id` now, spending nothing: when the
    /// link ends and the uses it has left.
    pub fn peek(
        &self,
        id: &GrantId,
        secret: &str,
        at: Now,
    ) -> Result<(Expiry, Option<u32>), Refused> {
        let presented = presented(secret);
        let Some(grant) = self.grants.get(id) else {
            return Err(match self.ended.get(id) {
                Some(&(why, _)) => Refused::Gone(Some(why)),
                None => Refused::NotFound,
            });
        };
        // Compared whatever was presented, so a malformed secret costs what a
        // wrong one does.
        let matched = same(&presented.unwrap_or([0; 32]), &grant.digest) && presented.is_some();
        if !matched {
            return Err(Refused::NotFound);
        }
        if grant.expires.passed(at) {
            return Err(Refused::Gone(Some(EndReason::Expired)));
        }
        if grant.uses_left == Some(0) {
            return Err(Refused::Gone(None));
        }
        Ok((grant.expires, grant.uses_left))
    }

    /// Spends one use of `id` for whoever presents its secret. Atomic: the
    /// caller holds the store, so two redemptions of a one-use link cannot
    /// both pass.
    pub fn redeem(&mut self, id: &GrantId, secret: &str, at: Now) -> Result<Redeemed, Refused> {
        self.peek(id, secret, at)?;
        let viewers = self.viewers.len();
        let of_link = self.viewers.values().filter(|v| v.grant == *id).count();
        let Some(grant) = self.grants.get_mut(id) else {
            return Err(Refused::NotFound);
        };
        if viewers >= MAX_VIEWERS || of_link >= MAX_VIEWERS_PER_LINK {
            return Err(Refused::Full);
        }
        // Minted before the use is spent: a failure spends nothing.
        let viewer = loop {
            let v = ViewerId(fresh_id().map_err(|_| Refused::Full)?);
            if !self.viewers.contains_key(&v) {
                break v;
            }
        };
        let (secret, digest) = fresh_secret().map_err(|_| Refused::Full)?;
        if let Some(left) = grant.uses_left.as_mut() {
            *left -= 1;
        }
        grant.redeemed += 1;
        let expires = grant.expires;
        self.viewers.insert(
            viewer.clone(),
            Viewer {
                grant: id.clone(),
                digest,
                live: None,
                offline_since: Some(at.mono),
                admitted: VecDeque::new(),
            },
        );
        Ok(Redeemed {
            cookie: format!("{}.{secret}", viewer.0),
            viewer,
            expires,
        })
    }

    /// The viewer a cookie names, if it is that viewer's and `grant`'s.
    fn credential(&self, grant: &GrantId, cookie: &str) -> Option<ViewerId> {
        let (viewer, secret) = cookie.split_once('.')?;
        let viewer = ViewerId::parse(viewer)?;
        let presented = presented(secret)?;
        let held = self.viewers.get(&viewer)?;
        (same(&presented, &held.digest) && held.grant == *grant).then_some(viewer)
    }

    /// When `cookie`'s link ends, if it may stream `grant` now.
    pub fn status(&self, grant: &GrantId, cookie: &str, at: Now) -> Option<Expiry> {
        self.credential(grant, cookie)?;
        let g = self.grants.get(grant).filter(|g| !g.expires.passed(at))?;
        Some(g.expires)
    }

    /// Admits `cookie` to stream `grant`: one live stream per credential.
    pub fn admit(&mut self, grant: &GrantId, cookie: &str, at: Now) -> Admit {
        let Some(viewer) = self.credential(grant, cookie) else {
            return Admit::Refused(Refused::NotFound);
        };
        let Some(g) = self.grants.get(grant).filter(|g| !g.expires.passed(at)) else {
            return Admit::Refused(Refused::Gone(Some(EndReason::Expired)));
        };
        let (session, expires) = (g.session, g.expires);
        let streaming = self
            .viewers
            .iter()
            .filter(|(id, v)| v.grant == *grant && v.live.is_some() && **id != viewer)
            .count();
        let stream = self.next_stream;
        let Some(held) = self.viewers.get_mut(&viewer) else {
            return Admit::Refused(Refused::NotFound);
        };
        // Throttled before anything else: a credential past its rate cannot
        // end its own live stream by asking for another.
        while held
            .admitted
            .front()
            .is_some_and(|&t| t + ADMIT_WINDOW <= at.mono)
        {
            held.admitted.pop_front();
        }
        if held.admitted.len() >= ADMIT_BURST {
            return Admit::Refused(Refused::Throttled);
        }
        if let Some(live) = &held.live {
            return Admit::Busy(live.clone());
        }
        if streaming >= MAX_STREAMS_PER_LINK {
            return Admit::Refused(Refused::Full);
        }
        held.admitted.push_back(at.mono);
        held.offline_since = None;
        self.next_stream += 1;
        let live = Live::new(stream);
        held.live = Some(live.clone());
        Admit::Ready(Admitted {
            viewer,
            session,
            expires,
            live,
        })
    }

    /// Takes back an admission whose stream never started: it costs the
    /// credential none of its reconnect budget.
    pub fn refund(&mut self, viewer: &ViewerId, stream: u64, at: Instant) {
        if let Some(held) = self.viewers.get_mut(viewer)
            && held.live.as_ref().is_some_and(|l| l.stream == stream)
        {
            held.live = None;
            held.offline_since = Some(at);
            held.admitted.pop_back();
        }
    }

    /// A stream has ended: its credential may stream again.
    pub fn stream_ended(&mut self, viewer: &ViewerId, stream: u64, at: Instant) {
        if let Some(held) = self.viewers.get_mut(viewer)
            && held.live.as_ref().is_some_and(|l| l.stream == stream)
        {
            held.live = None;
            held.offline_since = Some(at);
        }
    }

    /// Ends `id` and every viewer it admitted, live or not.
    pub fn revoke(&mut self, id: &GrantId, at: Instant) -> Option<Vec<Live>> {
        self.grants.remove(id)?;
        self.bury(id.clone(), EndReason::Revoked, at);
        Some(self.drop_viewers(|v| v.grant == *id, EndReason::Revoked))
    }

    /// Why `id` ended, while it is remembered.
    pub fn ended(&self, id: &GrantId) -> Option<EndReason> {
        self.ended.get(id).map(|&(why, _)| why)
    }

    /// Remembers that `id` ended and why, for [`ENDED_GRACE`] and among at
    /// most [`MAX_ENDED`]: no digest, use or viewer, only the reason.
    fn bury(&mut self, id: GrantId, why: EndReason, at: Instant) {
        self.ended
            .retain(|_, &mut (_, when)| when + ENDED_GRACE > at);
        while self.ended.len() >= MAX_ENDED {
            let oldest = self
                .ended
                .iter()
                .min_by_key(|(_, (_, when))| *when)
                .map(|(id, _)| id.clone());
            match oldest {
                Some(id) => self.ended.remove(&id),
                None => break,
            };
        }
        self.ended.insert(id, (why, at));
    }

    /// Drops one viewer; its link stays.
    pub fn kick(&mut self, viewer: &ViewerId) -> Option<Option<Live>> {
        let held = self.viewers.remove(viewer)?;
        Some(held.live.inspect(|l| l.end(EndReason::Kicked)))
    }

    /// Ends every link and viewer.
    pub fn stop(&mut self) -> Vec<Live> {
        self.end_all(EndReason::Stopped)
    }

    /// Ends every link and viewer for `why`.
    pub fn end_all(&mut self, why: EndReason) -> Vec<Live> {
        self.generation += 1;
        self.grants.clear();
        self.drop_viewers(|_| true, why)
    }

    /// Every live link and the session it shows.
    pub fn links(&self) -> HashMap<GrantId, SessionId> {
        self.grants
            .iter()
            .map(|(id, g)| (id.clone(), g.session))
            .collect()
    }

    /// The sessions a guest can still reach: through a link with a use left,
    /// or a credential that can reconnect. A spent link nobody holds is not
    /// one, though `list` shows it until it expires.
    pub fn reachable(&self) -> std::collections::HashSet<SessionId> {
        self.grants
            .iter()
            .filter(|(id, g)| {
                g.uses_left != Some(0) || self.viewers.values().any(|v| v.grant == **id)
            })
            .map(|(_, g)| g.session)
            .collect()
    }

    /// When the next link expires.
    pub fn next_expiry(&self) -> Option<Instant> {
        self.grants.values().map(|g| g.expires.mono).min()
    }

    /// Ends every link past its expiry, and every link to a session `gone`
    /// names, with all their viewers; drops credentials offline past
    /// [`OFFLINE_GRACE`].
    pub fn end_grants(&mut self, at: Now, gone: impl Fn(SessionId) -> bool) -> Vec<Live> {
        let mut ended: Vec<(GrantId, EndReason)> = Vec::new();
        self.grants.retain(|id, g| {
            let why = if g.expires.passed(at) {
                EndReason::Expired
            } else if gone(g.session) {
                EndReason::SessionEnded
            } else {
                return true;
            };
            ended.push((id.clone(), why));
            false
        });
        let mut lives = Vec::new();
        for (id, why) in ended {
            lives.extend(self.drop_viewers(|v| v.grant == id, why));
            self.bury(id, why, at.mono);
        }
        self.ended
            .retain(|_, &mut (_, when)| when + ENDED_GRACE > at.mono);
        // Only an unlimited link's credentials leave for idling: a limited
        // link's are its viewers' only way back in, and its uses bound them.
        let unlimited: Vec<GrantId> = self
            .grants
            .iter()
            .filter(|(_, g)| g.uses_left.is_none())
            .map(|(id, _)| id.clone())
            .collect();
        self.viewers.retain(|_, v| {
            !unlimited.contains(&v.grant)
                || v.offline_since
                    .is_none_or(|since| since + OFFLINE_GRACE > at.mono)
        });
        lives
    }

    fn drop_viewers(&mut self, which: impl Fn(&Viewer) -> bool, why: EndReason) -> Vec<Live> {
        let mut ended = Vec::new();
        self.viewers.retain(|_, v| {
            if !which(v) {
                return true;
            }
            if let Some(live) = v.live.take() {
                live.end(why);
                ended.push(live);
            }
            false
        });
        ended
    }

    pub fn list(&self) -> Vec<GrantInfo> {
        let mut out: Vec<GrantInfo> = self
            .grants
            .iter()
            .map(|(id, g)| {
                let mut viewers: Vec<ViewerInfo> = self
                    .viewers
                    .iter()
                    .filter(|(_, v)| v.grant == *id)
                    .map(|(vid, v)| ViewerInfo {
                        id: vid.clone(),
                        connected: v.live.as_ref().is_some_and(Live::drawing),
                    })
                    .collect();
                viewers.sort_by(|a, b| a.id.cmp(&b.id));
                GrantInfo {
                    id: id.clone(),
                    session: g.session,
                    label: g.label.clone(),
                    expires_wall: g.expires.wall,
                    expires: g.expires,
                    uses_left: g.uses_left,
                    redeemed: g.redeemed,
                    viewers,
                }
            })
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn session() -> SessionId {
        SessionId::new()
    }

    /// `mono` on the monotonic clock, and a wall clock that holds still.
    fn at(mono: Instant) -> Now {
        Now {
            mono,
            wall: SystemTime::UNIX_EPOCH + Duration::from_secs(1 << 30),
        }
    }

    fn mint(store: &mut Store, opts: CreateOpts, now: Now) -> Minted {
        store.create(opts, now).expect("mint")
    }

    #[test]
    fn ids_are_eight_base32_chars_and_secrets_256_bits() {
        let id = fresh_id().unwrap();
        assert!(valid_id(&id), "{id}");
        let (secret, digest) = fresh_secret().unwrap();
        assert_eq!(secret.len(), 43);
        assert_eq!(presented(&secret), Some(digest));
        assert_eq!(
            presented(&secret[..42]),
            None,
            "a short secret is no secret"
        );
        assert_ne!(fresh_secret().unwrap().0, secret);
    }

    #[test]
    fn the_store_keeps_no_secret_it_hands_out() {
        let now = at(Instant::now());
        let mut store = Store::default();
        let minted = mint(&mut store, CreateOpts::new(session()), now);
        let redeemed = store
            .redeem(&minted.id, &minted.secret, now)
            .expect("redeem");
        let dump = format!("{store:?}");
        let cookie_secret = redeemed.cookie.split_once('.').unwrap().1;
        assert!(!dump.contains(&minted.secret), "the link secret is held");
        assert!(!dump.contains(cookie_secret), "the cookie secret is held");
        // Nor does a debug print of what carries them.
        assert!(
            !format!("{minted:?}").contains(&minted.secret),
            "{minted:?}"
        );
        assert!(
            !format!("{redeemed:?}").contains(cookie_secret),
            "{redeemed:?}"
        );
    }

    #[test]
    fn a_link_is_one_use_unless_asked_and_its_secret_only_its_own() {
        let now = at(Instant::now());
        let mut store = Store::default();
        let a = mint(&mut store, CreateOpts::new(session()), now);
        let b = mint(&mut store, CreateOpts::new(session()), now);
        assert_eq!(
            store.redeem(&a.id, &b.secret, now).unwrap_err(),
            Refused::NotFound
        );
        assert_eq!(
            store.redeem(&a.id, "nope", now).unwrap_err(),
            Refused::NotFound
        );
        store.redeem(&a.id, &a.secret, now).expect("first use");
        assert_eq!(
            store.redeem(&a.id, &a.secret, now).unwrap_err(),
            Refused::Gone(None)
        );
    }

    #[test]
    fn expiry_ends_redemption_admission_and_the_link() {
        // why: an origin handed to the store, which reads no clock; later instants are data
        // ast-grep-ignore: test-wall-clock-unit
        let now = Instant::now();
        let mut store = Store::default();
        let opts = CreateOpts {
            uses: Uses::Unlimited,
            ttl: Duration::from_secs(60),
            ..CreateOpts::new(session())
        };
        let g = mint(&mut store, opts, at(now));
        let r = store.redeem(&g.id, &g.secret, at(now)).unwrap();
        let later = at(now + Duration::from_secs(60));
        assert_eq!(
            store.redeem(&g.id, &g.secret, later).unwrap_err(),
            Refused::Gone(Some(EndReason::Expired))
        );
        assert!(matches!(
            store.admit(&g.id, &r.cookie, later),
            Admit::Refused(Refused::Gone(_))
        ));
        assert!(
            store.end_grants(later, |_| false).is_empty(),
            "nothing was streaming"
        );
        assert!(store.list().is_empty());
    }

    /// An ended link says why to whoever opens it after, for a while: it
    /// expired, or its owner revoked it. Past the grace it is unknown again.
    #[test]
    fn an_ended_link_says_why_for_a_while() {
        // why: an origin handed to the store, which reads no clock; later instants are data
        // ast-grep-ignore: test-wall-clock-unit
        let now = Instant::now();
        let mut store = Store::default();
        let short = CreateOpts {
            ttl: Duration::from_secs(60),
            ..CreateOpts::new(session())
        };
        let expired = mint(&mut store, short, at(now));
        let revoked = mint(&mut store, CreateOpts::new(session()), at(now));
        store.revoke(&revoked.id, now);
        let later = at(now + Duration::from_secs(60));
        let _ = store.end_grants(later, |_| false);
        for (g, why) in [
            (&expired, EndReason::Expired),
            (&revoked, EndReason::Revoked),
        ] {
            assert_eq!(
                store.redeem(&g.id, &g.secret, later).unwrap_err(),
                Refused::Gone(Some(why))
            );
        }
        let forgotten = at(now + ENDED_GRACE + Duration::from_secs(60));
        let _ = store.end_grants(forgotten, |_| false);
        assert_eq!(
            store
                .redeem(&revoked.id, &revoked.secret, forgotten)
                .unwrap_err(),
            Refused::NotFound
        );
    }

    /// The monotonic clock stops while a laptop sleeps: an hour's link must
    /// not outlive eight hours of it.
    #[test]
    fn a_link_expires_by_the_wall_clock_through_a_sleep() {
        let mono = Instant::now();
        let before = at(mono);
        let mut store = Store::default();
        let g = mint(&mut store, CreateOpts::new(session()), before);
        let r = store.redeem(&g.id, &g.secret, before).unwrap();
        let Admit::Ready(a) = store.admit(&g.id, &r.cookie, before) else {
            panic!("not admitted");
        };
        let woke = Now {
            mono,
            wall: before.wall + Duration::from_secs(8 * 3600),
        };
        assert!(a.expires.passed(woke), "a stream would outlive its link");
        assert_eq!(store.status(&g.id, &r.cookie, woke), None);
        let ended = store.end_grants(woke, |_| false);
        assert_eq!(ended[0].reason(), Some(EndReason::Expired));
        assert!(store.list().is_empty());
    }

    #[test]
    fn ttl_and_label_are_bounded() {
        let now = at(Instant::now());
        let mut store = Store::default();
        let s = session();
        for (ttl, label, want) in [
            (Duration::ZERO, None, Err(CreateError::Ttl)),
            (
                MAX_TTL + Duration::from_secs(1),
                None,
                Err(CreateError::Ttl),
            ),
            (
                MAX_TTL,
                Some("x".repeat(MAX_LABEL + 1)),
                Err(CreateError::Label),
            ),
            (MAX_TTL, Some("a\u{1b}[2J".into()), Err(CreateError::Label)),
            (MAX_TTL, Some("reviewer".into()), Ok(())),
        ] {
            let opts = CreateOpts {
                ttl,
                label,
                ..CreateOpts::new(s)
            };
            let got = store.create(opts, now).map(|_| ());
            assert_eq!(got, want);
        }
    }

    #[test]
    fn the_daemon_holds_a_bounded_number_of_links() {
        // why: an origin handed to the store, which reads no clock; later instants are data
        // ast-grep-ignore: test-wall-clock-unit
        let now = Instant::now();
        let mut store = Store::default();
        for _ in 0..MAX_GRANTS {
            mint(&mut store, CreateOpts::new(session()), at(now));
        }
        let over = store.create(CreateOpts::new(session()), at(now));
        assert_eq!(over.unwrap_err(), CreateError::Full);
        // Expired links make room.
        let later = at(now + DEFAULT_TTL);
        assert!(store.create(CreateOpts::new(session()), later).is_ok());
    }

    /// An unlimited link takes at most its share of credentials and
    /// streams, and an idle credential gives its place back.
    #[test]
    fn one_link_holds_a_share_of_the_daemon_and_idle_credentials_leave() {
        // why: an origin handed to the store, which reads no clock; later instants are data
        // ast-grep-ignore: test-wall-clock-unit
        let now = Instant::now();
        let mut store = Store::default();
        let opts = CreateOpts {
            uses: Uses::Unlimited,
            ..CreateOpts::new(session())
        };
        let g = mint(&mut store, opts, at(now));
        let cookies: Vec<String> = (0..MAX_VIEWERS_PER_LINK)
            .map(|_| store.redeem(&g.id, &g.secret, at(now)).unwrap().cookie)
            .collect();
        assert_eq!(
            store.redeem(&g.id, &g.secret, at(now)).unwrap_err(),
            Refused::Full
        );
        let other = mint(&mut store, CreateOpts::new(session()), at(now));
        assert!(store.redeem(&other.id, &other.secret, at(now)).is_ok());
        let admitted: Vec<_> = cookies
            .iter()
            .map(|c| store.admit(&g.id, c, at(now)))
            .collect();
        let ready = admitted
            .iter()
            .filter(|a| matches!(a, Admit::Ready(_)))
            .count();
        assert_eq!(ready, MAX_STREAMS_PER_LINK);
        assert!(matches!(
            admitted[MAX_STREAMS_PER_LINK],
            Admit::Refused(Refused::Full)
        ));
        // Credentials with no stream leave after the grace period.
        let later = at(now + OFFLINE_GRACE);
        let _ = store.end_grants(later, |_| false);
        let listed = store.list();
        let held = listed
            .iter()
            .find(|i| i.id == g.id)
            .expect("listed")
            .viewers
            .len();
        assert_eq!(held, MAX_STREAMS_PER_LINK, "only the streaming ones stay");
        assert!(store.redeem(&g.id, &g.secret, later).is_ok());
    }

    #[test]
    fn a_credential_reconnects_only_so_often() {
        // why: an origin handed to the store, which reads no clock; later instants are data
        // ast-grep-ignore: test-wall-clock-unit
        let now = Instant::now();
        let mut store = Store::default();
        let g = mint(&mut store, CreateOpts::new(session()), at(now));
        let r = store.redeem(&g.id, &g.secret, at(now)).unwrap();
        for _ in 0..ADMIT_BURST {
            let Admit::Ready(a) = store.admit(&g.id, &r.cookie, at(now)) else {
                panic!("not admitted");
            };
            store.stream_ended(&a.viewer, a.live.stream(), now);
        }
        assert!(matches!(
            store.admit(&g.id, &r.cookie, at(now)),
            Admit::Refused(Refused::Throttled)
        ));
        assert!(matches!(
            store.admit(&g.id, &r.cookie, at(now + ADMIT_WINDOW)),
            Admit::Ready(_)
        ));
    }

    /// Time left is the nearer clock's, and a cookie with under a second
    /// of it left still lives: `Max-Age=0` would delete it.
    #[test]
    fn time_left_is_the_nearer_clocks_and_a_cookie_rounds_it_up() {
        // why: an origin handed to the store, which reads no clock; later instants are data
        // ast-grep-ignore: test-wall-clock-unit
        let before = at(Instant::now());
        let ttl = Duration::from_secs(1);
        let expires = Expiry {
            mono: before.mono + ttl,
            wall: before.wall + ttl,
        };
        let shift = Duration::from_millis(875);
        for now in [
            Now {
                mono: before.mono + shift,
                ..before
            },
            Now {
                wall: before.wall + shift,
                ..before
            },
        ] {
            assert_eq!(expires.remaining_ms(now), 125);
            assert_eq!(expires.cookie_max_age(now), 1);
        }
    }

    /// A throttled credential cannot end its own live stream by asking for
    /// another: the throttle answers before replacement does.
    #[test]
    fn a_throttled_reconnect_leaves_the_live_stream_alone() {
        let now = Instant::now();
        let mut store = Store::default();
        let g = mint(&mut store, CreateOpts::new(session()), at(now));
        let r = store.redeem(&g.id, &g.secret, at(now)).unwrap();
        for _ in 1..ADMIT_BURST {
            let Admit::Ready(a) = store.admit(&g.id, &r.cookie, at(now)) else {
                panic!("not admitted");
            };
            store.stream_ended(&a.viewer, a.live.stream(), now);
        }
        let Admit::Ready(live) = store.admit(&g.id, &r.cookie, at(now)) else {
            panic!("not admitted");
        };
        assert!(matches!(
            store.admit(&g.id, &r.cookie, at(now)),
            Admit::Refused(Refused::Throttled)
        ));
        assert!(!live.live.stop().is_cancelled());
    }

    /// A credential from a limited link is the viewer's only way back in:
    /// it is never dropped for idling. An unlimited link's are.
    #[test]
    fn a_limited_links_credential_outlives_an_idle_spell() {
        // why: an origin handed to the store, which reads no clock; later instants are data
        // ast-grep-ignore: test-wall-clock-unit
        let now = Instant::now();
        let mut store = Store::default();
        let limited = mint(&mut store, CreateOpts::new(session()), at(now));
        let kept = store.redeem(&limited.id, &limited.secret, at(now)).unwrap();
        let unlimited = CreateOpts {
            uses: Uses::Unlimited,
            ..CreateOpts::new(session())
        };
        let open = mint(&mut store, unlimited, at(now));
        let dropped = store.redeem(&open.id, &open.secret, at(now)).unwrap();
        let later = at(now + OFFLINE_GRACE);
        let _ = store.end_grants(later, |_| false);
        assert!(matches!(
            store.admit(&limited.id, &kept.cookie, later),
            Admit::Ready(_)
        ));
        assert!(matches!(
            store.admit(&open.id, &dropped.cookie, later),
            Admit::Refused(Refused::NotFound)
        ));
    }

    #[test]
    fn a_cookie_streams_once_at_a_time_and_only_its_own_link() {
        let mono = Instant::now();
        let now = at(mono);
        let mut store = Store::default();
        let s = session();
        let a = mint(&mut store, CreateOpts::new(s), now);
        let b = mint(&mut store, CreateOpts::new(s), now);
        let ra = store.redeem(&a.id, &a.secret, now).unwrap();
        assert!(matches!(
            store.admit(&b.id, &ra.cookie, now),
            Admit::Refused(Refused::NotFound)
        ));
        let Admit::Ready(first) = store.admit(&a.id, &ra.cookie, now) else {
            panic!("not admitted");
        };
        let Admit::Busy(live) = store.admit(&a.id, &ra.cookie, now) else {
            panic!("a second stream was admitted beside the first");
        };
        assert_eq!(live.stream(), first.live.stream());
        // A stale end of an older stream does not free a newer one.
        store.stream_ended(&first.viewer, first.live.stream() + 1, mono);
        assert!(matches!(
            store.admit(&a.id, &ra.cookie, now),
            Admit::Busy(_)
        ));
        store.stream_ended(&first.viewer, first.live.stream(), mono);
        assert!(matches!(
            store.admit(&a.id, &ra.cookie, now),
            Admit::Ready(_)
        ));
        // A forged secret under a real viewer id is refused.
        let forged = format!("{}.{}", ra.viewer.as_str(), a.secret);
        assert!(matches!(
            store.admit(&a.id, &forged, now),
            Admit::Refused(Refused::NotFound)
        ));
    }

    #[test]
    fn revoke_and_session_end_take_offline_viewers_too() {
        let now = at(Instant::now());
        let mut store = Store::default();
        let (s1, s2) = (session(), session());
        let unlimited = |s| CreateOpts {
            uses: Uses::Unlimited,
            ..CreateOpts::new(s)
        };
        let a = mint(&mut store, unlimited(s1), now);
        let b = mint(&mut store, unlimited(s2), now);
        let online = store.redeem(&a.id, &a.secret, now).unwrap();
        let offline = store.redeem(&a.id, &a.secret, now).unwrap();
        let other = store.redeem(&b.id, &b.secret, now).unwrap();
        let Admit::Ready(streaming) = store.admit(&a.id, &online.cookie, now) else {
            panic!("not admitted");
        };
        let ended = store.revoke(&a.id, now.mono).expect("revoked");
        assert_eq!(ended.len(), 1);
        assert_eq!(streaming.live.reason(), Some(EndReason::Revoked));
        assert!(streaming.live.stop().is_cancelled());
        assert!(matches!(
            store.admit(&a.id, &offline.cookie, now),
            Admit::Refused(_)
        ));
        assert!(store.revoke(&a.id, now.mono).is_none(), "revoked twice");
        // The other session's viewer is untouched until its session ends.
        assert!(matches!(
            store.admit(&b.id, &other.cookie, now),
            Admit::Ready(_)
        ));
        let ended = store.end_grants(now, |s| s == s2);
        assert_eq!(ended[0].reason(), Some(EndReason::SessionEnded));
        assert!(store.list().is_empty());
    }

    #[test]
    fn kick_drops_one_viewer_and_keeps_the_link() {
        let now = at(Instant::now());
        let mut store = Store::default();
        let opts = CreateOpts {
            uses: Uses::Limited(2.try_into().unwrap()),
            ..CreateOpts::new(session())
        };
        let g = mint(&mut store, opts, now);
        let r = store.redeem(&g.id, &g.secret, now).unwrap();
        let Admit::Ready(a) = store.admit(&g.id, &r.cookie, now) else {
            panic!("not admitted");
        };
        assert!(store.kick(&r.viewer).expect("kicked").is_some());
        assert_eq!(a.live.reason(), Some(EndReason::Kicked));
        assert!(matches!(
            store.admit(&g.id, &r.cookie, now),
            Admit::Refused(_)
        ));
        assert!(
            store.redeem(&g.id, &g.secret, now).is_ok(),
            "the link went with the viewer"
        );
    }

    #[derive(Debug, Clone)]
    enum Op {
        Create {
            uses: u8,
            ttl_s: u16,
        },
        Redeem {
            grant: usize,
            right: bool,
        },
        Admit {
            viewer: usize,
        },
        End {
            viewer: usize,
        },
        Revoke {
            grant: usize,
        },
        Kick {
            viewer: usize,
        },
        /// Both clocks run.
        Advance {
            s: u16,
        },
        /// Only the wall clock runs: the machine slept.
        Sleep {
            s: u16,
        },
        Stop,
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (0u8..4, 1u16..600).prop_map(|(uses, ttl_s)| Op::Create { uses, ttl_s }),
            (0usize..8, any::<bool>()).prop_map(|(grant, right)| Op::Redeem { grant, right }),
            (0usize..16).prop_map(|viewer| Op::Admit { viewer }),
            (0usize..16).prop_map(|viewer| Op::End { viewer }),
            (0usize..8).prop_map(|grant| Op::Revoke { grant }),
            (0usize..16).prop_map(|viewer| Op::Kick { viewer }),
            (0u16..300).prop_map(|s| Op::Advance { s }),
            (0u16..300).prop_map(|s| Op::Sleep { s }),
            Just(Op::Stop),
        ]
    }

    /// What the model holds of one link.
    struct MGrant {
        minted: Minted,
        allowed: Option<u32>,
        expires: Expiry,
        ended: bool,
    }

    /// What the model holds of one credential.
    struct MViewer {
        grant: usize,
        redeemed: Redeemed,
        live: Option<Live>,
        gone: bool,
        offline_since: Instant,
        admits: Vec<Instant>,
    }

    /// The store's sweep, in the model.
    fn sweep(grants: &mut [MGrant], viewers: &mut [MViewer], now: Now) {
        for g in grants.iter_mut() {
            g.ended |= g.expires.passed(now);
        }
        for v in viewers.iter_mut() {
            let idle = grants[v.grant].allowed.is_none()
                && v.live.is_none()
                && v.offline_since + OFFLINE_GRACE <= now.mono;
            if grants[v.grant].ended || idle {
                v.gone = true;
                v.live = None;
            }
        }
    }

    proptest! {
        /// Against a model of what each link allows: a link never admits more
        /// redemptions than its uses or its share of the daemon, an ended link
        /// or a dropped credential never streams again, no credential streams
        /// twice at once or reconnects past its rate, and expiry passes on
        /// either clock.
        #[test]
        fn the_store_never_admits_what_its_links_forbid(ops in proptest::collection::vec(op(), 1..80)) {
            let mut now = at(Instant::now());
            let mut store = Store::default();
            let s = session();
            let mut grants: Vec<MGrant> = Vec::new();
            let mut viewers: Vec<MViewer> = Vec::new();
            for op in ops {
                match op {
                    Op::Create { uses, ttl_s } => {
                        let ttl = Duration::from_secs(u64::from(ttl_s));
                        let opts = CreateOpts {
                            ttl,
                            uses: std::num::NonZeroU32::new(u32::from(uses)).map_or(Uses::Unlimited, Uses::Limited),
                            ..CreateOpts::new(s)
                        };
                        let m = store.create(opts, now).expect("under the cap");
                        sweep(&mut grants, &mut viewers, now);
                        let expires = Expiry { mono: now.mono + ttl, wall: now.wall + ttl };
                        grants.push(MGrant { minted: m, allowed: (uses > 0).then_some(u32::from(uses)), expires, ended: false });
                    }
                    Op::Redeem { grant, right } => {
                        let Some(i) = (!grants.is_empty()).then(|| grant % grants.len()) else { continue };
                        let g = &grants[i];
                        let secret = if right { g.minted.secret.clone() } else { fresh_secret().unwrap().0 };
                        let spent = viewers.iter().filter(|v| v.grant == i).count() as u32;
                        let held = viewers.iter().filter(|v| v.grant == i && !v.gone).count();
                        let open = !g.ended && !g.expires.passed(now) && g.allowed.is_none_or(|n| spent < n);
                        match store.redeem(&g.minted.id, &secret, now) {
                            Ok(r) => {
                                prop_assert!(right && open && held < MAX_VIEWERS_PER_LINK, "redeemed a link that forbids it");
                                viewers.push(MViewer { grant: i, redeemed: r, live: None, gone: false, offline_since: now.mono, admits: Vec::new() });
                            }
                            Err(Refused::NotFound) => prop_assert!(!right || g.ended),
                            Err(Refused::Gone(None)) => prop_assert!(right && !g.ended && !open),
                            // An ended link says why, whatever secret: it holds none.
                            Err(Refused::Gone(Some(_))) => prop_assert!(g.ended),
                            Err(Refused::Full) => prop_assert!(open && held >= MAX_VIEWERS_PER_LINK),
                            Err(Refused::Throttled) => prop_assert!(false, "a redeem is never throttled"),
                        }
                    }
                    Op::Admit { viewer } => {
                        let Some(i) = (!viewers.is_empty()).then(|| viewer % viewers.len()) else { continue };
                        let g = viewers[i].grant;
                        let others = viewers.iter().enumerate()
                            .filter(|(j, v)| *j != i && v.grant == g && v.live.is_some())
                            .count();
                        let v = &mut viewers[i];
                        let may = !v.gone && !grants[g].ended && !grants[g].expires.passed(now);
                        v.admits.retain(|&t| t + ADMIT_WINDOW > now.mono);
                        match store.admit(&grants[g].minted.id, &v.redeemed.cookie, now) {
                            Admit::Ready(a) => {
                                prop_assert!(may && v.live.is_none(), "admitted a credential that may not stream");
                                prop_assert!(v.admits.len() < ADMIT_BURST && others < MAX_STREAMS_PER_LINK);
                                v.admits.push(now.mono);
                                v.live = Some(a.live);
                            }
                            Admit::Busy(l) => {
                                prop_assert!(may && v.admits.len() < ADMIT_BURST);
                                prop_assert_eq!(v.live.as_ref().map(Live::stream), Some(l.stream()));
                            }
                            Admit::Refused(Refused::Throttled) => prop_assert!(may && v.admits.len() >= ADMIT_BURST),
                            Admit::Refused(Refused::Full) => prop_assert!(may && others >= MAX_STREAMS_PER_LINK),
                            Admit::Refused(_) => prop_assert!(!may),
                        }
                    }
                    Op::End { viewer } => {
                        let Some(i) = (!viewers.is_empty()).then(|| viewer % viewers.len()) else { continue };
                        if let Some(l) = viewers[i].live.take() {
                            store.stream_ended(&viewers[i].redeemed.viewer, l.stream(), now.mono);
                            viewers[i].offline_since = now.mono;
                        }
                    }
                    Op::Revoke { grant } => {
                        let Some(i) = (!grants.is_empty()).then(|| grant % grants.len()) else { continue };
                        let got = store.revoke(&grants[i].minted.id, now.mono);
                        prop_assert_eq!(got.is_some(), !grants[i].ended);
                        grants[i].ended = true;
                        for v in viewers.iter_mut().filter(|v| v.grant == i) {
                            v.gone = true;
                            if let Some(l) = v.live.take() {
                                prop_assert!(l.stop().is_cancelled(), "a revoked viewer kept streaming");
                            }
                        }
                    }
                    Op::Kick { viewer } => {
                        let Some(i) = (!viewers.is_empty()).then(|| viewer % viewers.len()) else { continue };
                        let _ = store.kick(&viewers[i].redeemed.viewer);
                        viewers[i].gone = true;
                        if let Some(l) = viewers[i].live.take() {
                            prop_assert!(l.stop().is_cancelled());
                        }
                    }
                    Op::Advance { s } | Op::Sleep { s } => {
                        let d = Duration::from_secs(u64::from(s));
                        if matches!(op, Op::Advance { .. }) {
                            now.mono += d;
                        }
                        now.wall += d;
                        for l in store.end_grants(now, |_| false) {
                            prop_assert_eq!(l.reason(), Some(EndReason::Expired));
                        }
                        sweep(&mut grants, &mut viewers, now);
                    }
                    Op::Stop => {
                        for l in store.stop() {
                            prop_assert!(l.stop().is_cancelled());
                        }
                        for g in grants.iter_mut() {
                            g.ended = true;
                        }
                        for v in viewers.iter_mut() {
                            v.gone = true;
                            v.live = None;
                        }
                    }
                }
                // The listing agrees with the model.
                for info in store.list() {
                    let (i, g) = grants.iter().enumerate().find(|(_, g)| g.minted.id == info.id).expect("modelled");
                    prop_assert!(!g.ended);
                    if let Some(left) = info.uses_left {
                        prop_assert_eq!(left + info.redeemed, g.allowed.expect("limited"));
                    }
                    let held = viewers.iter().filter(|v| v.grant == i && !v.gone).count();
                    prop_assert_eq!(info.viewers.len(), held);
                }
            }
        }
    }
}
