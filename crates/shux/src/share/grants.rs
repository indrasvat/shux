//! Share links and the viewers they admitted: the gateway's whole state.
//!
//! A link is `https://<origin>/v/<id>#<secret>`. Only the SHA-256 of a secret
//! is kept, and a presented one is compared in constant time. Redeeming a
//! link spends one of its uses and mints a viewer credential of its own (a
//! cookie), so one viewer can be dropped without ending the link. Every
//! operation takes the clock as an argument: the store decides nothing by
//! reading time itself.

use std::collections::HashMap;
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
pub const MAX_LABEL: usize = 64;

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
        }
    }
}

/// One live stream of one viewer: ended through `stop`, and `done` once its
/// socket is gone.
#[derive(Debug, Clone)]
pub struct Live {
    stream: u64,
    stop: CancellationToken,
    reason: Arc<OnceLock<EndReason>>,
    done: CancellationToken,
}

impl Live {
    fn new(stream: u64) -> Self {
        Self {
            stream,
            stop: CancellationToken::new(),
            reason: Arc::default(),
            done: CancellationToken::new(),
        }
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
    expires: Instant,
    expires_wall: SystemTime,
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
}

/// A link just minted. `secret` exists only here.
#[derive(Debug, Clone)]
pub struct Minted {
    pub id: GrantId,
    pub secret: String,
    pub expires_wall: SystemTime,
}

/// A redemption that succeeded: the cookie value to set, once.
#[derive(Debug, Clone)]
pub struct Redeemed {
    pub viewer: ViewerId,
    pub cookie: String,
    pub expires: Instant,
    pub expires_wall: SystemTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// No such link, or not its secret: nothing is said about which.
    NotFound,
    /// The right secret for a link that has ended or has no uses left.
    Gone,
    /// The daemon holds as many viewers as it will, or cannot mint one.
    Full,
}

/// A credential admitted to stream.
#[derive(Debug, Clone)]
pub struct Admitted {
    pub viewer: ViewerId,
    pub grant: GrantId,
    pub session: SessionId,
    pub expires: Instant,
    pub expires_wall: SystemTime,
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
}

impl Store {
    pub fn create(
        &mut self,
        opts: CreateOpts,
        now: Instant,
        wall: SystemTime,
    ) -> Result<Minted, CreateError> {
        if opts.ttl.is_zero() || opts.ttl > MAX_TTL {
            return Err(CreateError::Ttl);
        }
        if let Some(label) = &opts.label
            && (label.chars().count() > MAX_LABEL || label.chars().any(char::is_control))
        {
            return Err(CreateError::Label);
        }
        self.sweep(now);
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
        let expires_wall = wall + opts.ttl;
        self.grants.insert(
            id.clone(),
            Grant {
                session: opts.session,
                digest,
                expires: now + opts.ttl,
                expires_wall,
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
            expires_wall,
        })
    }

    /// Spends one use of `id` for whoever presents its secret. Atomic: the
    /// caller holds the store, so two redemptions of a one-use link cannot
    /// both pass.
    pub fn redeem(
        &mut self,
        id: &GrantId,
        secret: &str,
        now: Instant,
    ) -> Result<Redeemed, Refused> {
        let presented = presented(secret);
        let viewers = self.viewers.len();
        let grant = self.grants.get_mut(id).ok_or(Refused::NotFound)?;
        // Compared whatever was presented, so a malformed secret costs what a
        // wrong one does.
        let matched = same(&presented.unwrap_or([0; 32]), &grant.digest) && presented.is_some();
        if !matched {
            return Err(Refused::NotFound);
        }
        if grant.expires <= now || grant.uses_left == Some(0) {
            return Err(Refused::Gone);
        }
        if viewers >= MAX_VIEWERS {
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
        let (expires, expires_wall) = (grant.expires, grant.expires_wall);
        self.viewers.insert(
            viewer.clone(),
            Viewer {
                grant: id.clone(),
                digest,
                live: None,
            },
        );
        Ok(Redeemed {
            cookie: format!("{}.{secret}", viewer.0),
            viewer,
            expires,
            expires_wall,
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

    /// Whether `cookie` may stream `grant` now, without admitting it.
    pub fn status(
        &self,
        grant: &GrantId,
        cookie: &str,
        now: Instant,
    ) -> Option<(SessionId, SystemTime)> {
        self.credential(grant, cookie)?;
        let g = self.grants.get(grant).filter(|g| g.expires > now)?;
        Some((g.session, g.expires_wall))
    }

    /// Admits `cookie` to stream `grant`: one live stream per credential.
    pub fn admit(&mut self, grant: &GrantId, cookie: &str, now: Instant) -> Admit {
        let Some(viewer) = self.credential(grant, cookie) else {
            return Admit::Refused(Refused::NotFound);
        };
        let Some(g) = self.grants.get(grant).filter(|g| g.expires > now) else {
            return Admit::Refused(Refused::Gone);
        };
        let (session, expires, expires_wall) = (g.session, g.expires, g.expires_wall);
        let stream = self.next_stream;
        let Some(held) = self.viewers.get_mut(&viewer) else {
            return Admit::Refused(Refused::NotFound);
        };
        if let Some(live) = &held.live {
            return Admit::Busy(live.clone());
        }
        self.next_stream += 1;
        let live = Live::new(stream);
        held.live = Some(live.clone());
        Admit::Ready(Admitted {
            viewer,
            grant: grant.clone(),
            session,
            expires,
            expires_wall,
            live,
        })
    }

    /// A stream has ended: its credential may stream again.
    pub fn stream_ended(&mut self, viewer: &ViewerId, stream: u64) {
        if let Some(held) = self.viewers.get_mut(viewer)
            && held.live.as_ref().is_some_and(|l| l.stream == stream)
        {
            held.live = None;
        }
    }

    /// Ends `id` and every viewer it admitted, live or not.
    pub fn revoke(&mut self, id: &GrantId) -> Option<Vec<Live>> {
        self.grants.remove(id)?;
        Some(self.drop_viewers(|v| v.grant == *id, EndReason::Revoked))
    }

    /// Drops one viewer; its link stays.
    pub fn kick(&mut self, viewer: &ViewerId) -> Option<Option<Live>> {
        let held = self.viewers.remove(viewer)?;
        Some(held.live.inspect(|l| l.end(EndReason::Kicked)))
    }

    /// Ends every link and viewer.
    pub fn stop(&mut self) -> Vec<Live> {
        self.grants.clear();
        self.drop_viewers(|_| true, EndReason::Stopped)
    }

    /// Ends every link to `session` and every viewer they admitted.
    pub fn end_session(&mut self, session: SessionId) -> Vec<Live> {
        let gone: Vec<GrantId> = self
            .grants
            .iter()
            .filter(|(_, g)| g.session == session)
            .map(|(id, _)| id.clone())
            .collect();
        self.grants.retain(|_, g| g.session != session);
        self.drop_viewers(|v| gone.contains(&v.grant), EndReason::SessionEnded)
    }

    /// Drops every link past its expiry, with its viewers.
    pub fn sweep(&mut self, now: Instant) -> Vec<Live> {
        let gone: Vec<GrantId> = self
            .grants
            .iter()
            .filter(|(_, g)| g.expires <= now)
            .map(|(id, _)| id.clone())
            .collect();
        if gone.is_empty() {
            return Vec::new();
        }
        self.grants.retain(|id, _| !gone.contains(id));
        self.drop_viewers(|v| gone.contains(&v.grant), EndReason::Expired)
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

    /// Sessions some link still points at.
    pub fn sessions(&self) -> Vec<SessionId> {
        let mut s: Vec<SessionId> = self.grants.values().map(|g| g.session).collect();
        s.sort_unstable_by_key(ToString::to_string);
        s.dedup();
        s
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
                        connected: v.live.is_some(),
                    })
                    .collect();
                viewers.sort_by(|a, b| a.id.cmp(&b.id));
                GrantInfo {
                    id: id.clone(),
                    session: g.session,
                    label: g.label.clone(),
                    expires_wall: g.expires_wall,
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

    fn mint(store: &mut Store, opts: CreateOpts, now: Instant) -> Minted {
        store
            .create(opts, now, SystemTime::UNIX_EPOCH)
            .expect("mint")
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
        let now = Instant::now();
        let mut store = Store::default();
        let minted = mint(&mut store, CreateOpts::new(session()), now);
        let redeemed = store
            .redeem(&minted.id, &minted.secret, now)
            .expect("redeem");
        let dump = format!("{store:?}");
        let cookie_secret = redeemed.cookie.split_once('.').unwrap().1;
        assert!(!dump.contains(&minted.secret), "the link secret is held");
        assert!(!dump.contains(cookie_secret), "the cookie secret is held");
    }

    #[test]
    fn a_link_is_one_use_unless_asked_and_its_secret_only_its_own() {
        let now = Instant::now();
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
            Refused::Gone
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
        let g = mint(&mut store, opts, now);
        let r = store.redeem(&g.id, &g.secret, now).unwrap();
        let later = now + Duration::from_secs(60);
        assert_eq!(
            store.redeem(&g.id, &g.secret, later).unwrap_err(),
            Refused::Gone
        );
        assert!(matches!(
            store.admit(&g.id, &r.cookie, later),
            Admit::Refused(Refused::Gone)
        ));
        assert!(store.sweep(later).is_empty(), "nothing was streaming");
        assert!(store.list().is_empty());
    }

    #[test]
    fn ttl_and_label_are_bounded() {
        let now = Instant::now();
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
            let got = store.create(opts, now, SystemTime::UNIX_EPOCH).map(|_| ());
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
            mint(&mut store, CreateOpts::new(session()), now);
        }
        let over = store.create(CreateOpts::new(session()), now, SystemTime::UNIX_EPOCH);
        assert_eq!(over.unwrap_err(), CreateError::Full);
        // Expired links make room.
        let later = now + DEFAULT_TTL;
        assert!(
            store
                .create(CreateOpts::new(session()), later, SystemTime::UNIX_EPOCH)
                .is_ok()
        );
    }

    #[test]
    fn a_cookie_streams_once_at_a_time_and_only_its_own_link() {
        let now = Instant::now();
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
        store.stream_ended(&first.viewer, first.live.stream() + 1);
        assert!(matches!(
            store.admit(&a.id, &ra.cookie, now),
            Admit::Busy(_)
        ));
        store.stream_ended(&first.viewer, first.live.stream());
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
        let now = Instant::now();
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
        let ended = store.revoke(&a.id).expect("revoked");
        assert_eq!(ended.len(), 1);
        assert_eq!(streaming.live.reason(), Some(EndReason::Revoked));
        assert!(streaming.live.stop().is_cancelled());
        assert!(matches!(
            store.admit(&a.id, &offline.cookie, now),
            Admit::Refused(_)
        ));
        assert!(store.revoke(&a.id).is_none(), "revoked twice");
        // The other session's viewer is untouched until its session ends.
        assert!(matches!(
            store.admit(&b.id, &other.cookie, now),
            Admit::Ready(_)
        ));
        let ended = store.end_session(s2);
        assert_eq!(ended[0].reason(), Some(EndReason::SessionEnded));
        assert!(store.list().is_empty());
    }

    #[test]
    fn kick_drops_one_viewer_and_keeps_the_link() {
        let now = Instant::now();
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
        Create { uses: u8, ttl_s: u16 },
        Redeem { grant: usize, right: bool },
        Admit { viewer: usize },
        End { viewer: usize },
        Revoke { grant: usize },
        Kick { viewer: usize },
        Advance { s: u16 },
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
            Just(Op::Stop),
        ]
    }

    proptest! {
        /// Against a model of what each link allows: a link never admits
        /// more redemptions than its uses, an ended link or a kicked viewer
        /// never streams again, and no credential streams twice at once.
        #[test]
        fn the_store_never_admits_what_its_links_forbid(ops in proptest::collection::vec(op(), 1..80)) {
            let t0 = Instant::now();
            let mut now = t0;
            let mut store = Store::default();
            let s = session();
            // (minted, uses allowed, expiry, ended)
            let mut grants: Vec<(Minted, Option<u32>, Instant, bool)> = Vec::new();
            // (grant index, redeemed, live stream, kicked)
            let mut viewers: Vec<(usize, Redeemed, Option<Live>, bool)> = Vec::new();
            for op in ops {
                match op {
                    Op::Create { uses, ttl_s } => {
                        let ttl = Duration::from_secs(u64::from(ttl_s));
                        let opts = CreateOpts {
                            ttl,
                            uses: std::num::NonZeroU32::new(u32::from(uses)).map_or(Uses::Unlimited, Uses::Limited),
                            ..CreateOpts::new(s)
                        };
                        let m = store.create(opts, now, SystemTime::UNIX_EPOCH).expect("under the cap");
                        let allowed = (uses > 0).then_some(u32::from(uses));
                        grants.push((m, allowed, now + ttl, false));
                    }
                    Op::Redeem { grant, right } => {
                        let Some(i) = (!grants.is_empty()).then(|| grant % grants.len()) else { continue };
                        let (m, allowed, expires, ended) = &grants[i];
                        let secret = if right { m.secret.clone() } else { fresh_secret().unwrap().0 };
                        let spent = viewers.iter().filter(|v| v.0 == i).count() as u32;
                        let open = !ended && *expires > now && allowed.is_none_or(|n| spent < n);
                        match store.redeem(&m.id, &secret, now) {
                            Ok(r) => {
                                prop_assert!(right && open, "redeemed a link that forbids it");
                                viewers.push((i, r, None, false));
                            }
                            Err(Refused::NotFound) => prop_assert!(!right || *ended || *expires <= now),
                            Err(Refused::Gone) => prop_assert!(right && !open),
                            Err(Refused::Full) => prop_assert!(false, "no cap is near"),
                        }
                    }
                    Op::Admit { viewer } => {
                        let Some(i) = (!viewers.is_empty()).then(|| viewer % viewers.len()) else { continue };
                        let (g, r, live, kicked) = &viewers[i];
                        let (m, _, expires, ended) = &grants[*g];
                        let may = !kicked && !ended && *expires > now;
                        match store.admit(&m.id, &r.cookie, now) {
                            Admit::Ready(a) => {
                                prop_assert!(may && live.is_none(), "admitted a credential that may not stream");
                                viewers[i].2 = Some(a.live);
                            }
                            Admit::Busy(l) => {
                                prop_assert!(may);
                                prop_assert_eq!(live.as_ref().map(Live::stream), Some(l.stream()));
                            }
                            Admit::Refused(_) => prop_assert!(!may),
                        }
                    }
                    Op::End { viewer } => {
                        let Some(i) = (!viewers.is_empty()).then(|| viewer % viewers.len()) else { continue };
                        if let Some(l) = viewers[i].2.take() {
                            store.stream_ended(&viewers[i].1.viewer, l.stream());
                        }
                    }
                    Op::Revoke { grant } => {
                        let Some(i) = (!grants.is_empty()).then(|| grant % grants.len()) else { continue };
                        let was_live = !grants[i].3 && grants[i].2 > now;
                        let got = store.revoke(&grants[i].0.id);
                        // A link past expiry may already be swept away.
                        prop_assert!(got.is_some() || !was_live);
                        grants[i].3 = true;
                        for v in viewers.iter_mut().filter(|v| v.0 == i) {
                            if let Some(l) = v.2.take() {
                                prop_assert!(l.stop().is_cancelled(), "a revoked viewer kept streaming");
                            }
                        }
                    }
                    Op::Kick { viewer } => {
                        let Some(i) = (!viewers.is_empty()).then(|| viewer % viewers.len()) else { continue };
                        let _ = store.kick(&viewers[i].1.viewer);
                        viewers[i].3 = true;
                        if let Some(l) = viewers[i].2.take() {
                            prop_assert!(l.stop().is_cancelled());
                        }
                    }
                    Op::Advance { s } => {
                        now += Duration::from_secs(u64::from(s));
                        for l in store.sweep(now) {
                            prop_assert_eq!(l.reason(), Some(EndReason::Expired));
                        }
                        for v in viewers.iter_mut() {
                            if grants[v.0].2 <= now {
                                v.2 = None;
                            }
                        }
                    }
                    Op::Stop => {
                        for l in store.stop() {
                            prop_assert!(l.stop().is_cancelled());
                        }
                        for g in grants.iter_mut() {
                            g.3 = true;
                        }
                        for v in viewers.iter_mut() {
                            v.2 = None;
                        }
                    }
                }
                // No credential has two streams; listed counts match.
                for info in store.list() {
                    let live = info.viewers.iter().filter(|v| v.connected).count();
                    prop_assert!(live <= info.viewers.len());
                    if let Some(left) = info.uses_left {
                        let (_, allowed, _, _) = grants.iter().find(|g| g.0.id == info.id).expect("modelled");
                        prop_assert_eq!(left + info.redeemed, allowed.expect("limited"));
                    }
                }
            }
        }
    }
}
