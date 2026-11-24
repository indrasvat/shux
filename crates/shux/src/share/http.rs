//! The gateway's HTTP surface. Every route checks the exact public `Host`,
//! and an absolute-form request the exact authority; redemption and the
//! WebSocket also check the exact `Origin`. Every response this code makes
//! carries the same strict headers, and none is cached. (Hyper answers a
//! request it cannot parse itself, with a bare 400 or 431 that bypasses them.)

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio_util::sync::CancellationToken;

use super::grants::{Admit, GrantId, Refused};
use super::{Gateway, Origin, StreamGuard, TEARDOWN, rfc3339};

/// HTTP connections open at once. At the cap, the oldest whose request has
/// no answer yet is closed for the newest; one being answered (a response
/// on its way, an upgrade) never is. Each connection serves one request.
const MAX_CONNECTIONS: usize = 64;
const HEADER_READ: Duration = Duration::from_secs(10);
/// A connection's life until its request is answered, a slow body
/// included; and its whole life once it is, a slow download included.
const CONN_DEADLINE: Duration = Duration::from_secs(10);
const ANSWERED_DEADLINE: Duration = Duration::from_secs(60);
/// The pause after an accept fails for want of a resource, not a peer.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);
const MAX_BODY: usize = 512;
/// What a viewer's socket may hold unsent: a little over the window's worth of
/// chunks. A viewer whose pongs or frames pile up past it is dropped.
const WRITE_BUFFER: usize = 64 * 1024;
pub(super) const MAX_WRITE_BUFFER: usize = super::wire::WINDOW as usize + WRITE_BUFFER;

/// The socket settings `stream` gives axum, for tests that drive
/// tungstenite directly.
#[cfg(test)]
pub(super) fn ws_config() -> tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
    tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(super::wire::MAX_INBOUND))
        .max_frame_size(Some(super::wire::MAX_INBOUND))
        .write_buffer_size(WRITE_BUFFER)
        .max_write_buffer_size(MAX_WRITE_BUFFER)
}

const CSP: &str = "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; \
    style-src 'self'; connect-src 'self'; img-src 'self'; font-src 'self'; \
    base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

const PAGE: &str = include_str!("assets/viewer.html");

struct Asset {
    name: &'static str,
    kind: &'static str,
    body: &'static [u8],
}

const ASSETS: &[Asset] = &[
    Asset {
        name: "viewer.js",
        kind: "text/javascript; charset=utf-8",
        body: include_bytes!("assets/viewer.js"),
    },
    Asset {
        name: "viewer.css",
        kind: "text/css; charset=utf-8",
        body: include_bytes!("assets/viewer.css"),
    },
    Asset {
        name: "favicon.svg",
        kind: "image/svg+xml",
        body: include_bytes!("assets/favicon.svg"),
    },
    Asset {
        name: "ghostty-web.js",
        kind: "text/javascript; charset=utf-8",
        body: include_bytes!("assets/ghostty-web-0.4.0/ghostty-web.js"),
    },
    Asset {
        name: "__vite-browser-external-2447137e.js",
        kind: "text/javascript; charset=utf-8",
        body: include_bytes!("assets/ghostty-web-0.4.0/__vite-browser-external-2447137e.js"),
    },
    Asset {
        name: "ghostty-vt.wasm",
        kind: "application/wasm",
        body: include_bytes!("assets/ghostty-web-0.4.0/ghostty-vt.wasm"),
    },
];

/// An accept that failed for this one connection, not for the listener.
fn one_peer(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::*;
    matches!(
        e.kind(),
        ConnectionAborted | ConnectionReset | ConnectionRefused | Interrupted | WouldBlock
    )
}

/// Whether a connection may still be evicted. A request claims its
/// connection before it commits anything (a redeem, an admission) and when
/// it has an answer; eviction claims it first or not at all, so no request
/// commits on a connection being closed. A kept connection has
/// [`ANSWERED_DEADLINE`] to finish.
#[derive(Clone, Default)]
struct Answered(Arc<AtomicU8>);

const OPEN: u8 = 0;
const KEPT: u8 = 1;
const EVICTED: u8 = 2;

impl Answered {
    /// Claims the connection; false once it has been evicted.
    fn keep(&self) -> bool {
        matches!(
            self.0
                .compare_exchange(OPEN, KEPT, Ordering::SeqCst, Ordering::SeqCst),
            Ok(_) | Err(KEPT)
        )
    }
    /// Claims it for eviction; false once a request has kept it.
    fn evict(&self) -> bool {
        self.0
            .compare_exchange(OPEN, EVICTED, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
    fn kept(&self) -> bool {
        self.0.load(Ordering::SeqCst) == KEPT
    }
}

/// Closes a connection from outside its task: how a stream cut off before
/// its upgrade completes takes the socket HTTP still holds.
#[derive(Clone, Default)]
struct Cut(CancellationToken);

/// Until the stream admitted on this connection is done, its cutoff closes
/// the connection: a revoke, kick, stop, replacement or expiry that lands
/// while the upgrade is still pending ends it there, and no stream starts.
fn cut_on_stop(live: &super::grants::Live, cut: Option<&axum::Extension<Cut>>) {
    let Some(axum::Extension(Cut(cut))) = cut else {
        return;
    };
    let (stop, done, cut) = (live.stop().clone(), live.done().clone(), cut.clone());
    tokio::spawn(async move {
        tokio::select! {
            biased;
            () = done.cancelled() => {}
            () = stop.cancelled() => cut.cancel(),
        }
    });
}

/// A request's claim on its connection before it commits; a request with
/// no connection behind it (none in tests) has nothing to claim.
fn claim(conn: Option<&axum::Extension<Answered>>) -> bool {
    conn.is_none_or(|c| c.0.keep())
}

struct Conn {
    handle: tokio::task::AbortHandle,
    answered: Answered,
}

/// Room for one more connection at the cap: the oldest still waiting on its
/// own request makes way. False when every one is being answered.
fn make_room(open: &mut VecDeque<Conn>) -> bool {
    open.retain(|c| !c.handle.is_finished());
    if open.len() < MAX_CONNECTIONS {
        return true;
    }
    let Some(i) = open.iter().position(|c| c.answered.evict()) else {
        return false;
    };
    if let Some(c) = open.remove(i) {
        c.handle.abort();
    }
    true
}

pub(super) async fn serve(
    listener: tokio::net::TcpListener,
    gw: Gateway,
    cancel: CancellationToken,
) {
    let app = router(gw);
    let mut conns = tokio::task::JoinSet::new();
    let mut open: VecDeque<Conn> = VecDeque::new();
    loop {
        let accepted = tokio::select! {
            () = cancel.cancelled() => break,
            Some(_) = conns.join_next(), if !conns.is_empty() => continue,
            accepted = listener.accept() => accepted,
        };
        let tcp = match accepted {
            Ok((tcp, _)) => tcp,
            Err(e) if one_peer(&e) => continue,
            Err(e) => {
                tracing::warn!(error = %e, "share: accept failed");
                #[expect(
                    clippy::disallowed_methods,
                    reason = "backs off a failing listener; never synchronises a test"
                )]
                let pause = tokio::time::sleep(ACCEPT_BACKOFF);
                tokio::select! {
                    () = cancel.cancelled() => break,
                    () = pause => continue,
                }
            }
        };
        if !make_room(&mut open) {
            // Every connection is being answered: this one is refused.
            continue;
        }
        let (answered, cut) = (Answered::default(), Cut::default());
        let service = hyper_util::service::TowerToHyperService::new(
            app.clone()
                .layer(axum::Extension(answered.clone()))
                .layer(axum::Extension(cut.clone())),
        );
        let cancel = cancel.clone();
        let flag = answered.clone();
        let handle = conns.spawn(async move {
            let conn = hyper::server::conn::http1::Builder::new()
                .keep_alive(false)
                .timer(TokioTimer::new())
                .header_read_timeout(HEADER_READ)
                .serve_connection(TokioIo::new(tcp), service)
                .with_upgrades();
            #[expect(
                clippy::disallowed_methods,
                reason = "a connection's own deadline; never synchronises a test"
            )]
            let deadline = async {
                tokio::time::sleep(CONN_DEADLINE).await;
                if flag.kept() {
                    tokio::time::sleep(ANSWERED_DEADLINE - CONN_DEADLINE).await;
                }
            };
            tokio::select! {
                () = cancel.cancelled() => {}
                () = cut.0.cancelled() => {}
                _ = conn => {}
                () = deadline => {}
            }
        });
        open.push_back(Conn { handle, answered });
    }
    conns.shutdown().await;
}

fn router(gw: Gateway) -> Router {
    Router::new()
        .route("/v/{id}", get(page))
        .route("/v/{id}/redeem", post(redeem))
        .route("/v/{id}/status", get(status))
        .route("/v/{id}/ws", get(stream))
        .route("/_/{name}", get(asset))
        .fallback(|| async { not_found() })
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(axum::middleware::from_fn_with_state(gw.clone(), authority))
        .layer(axum::middleware::map_response(harden))
        .layer(axum::middleware::from_fn(answer))
        .with_state(gw)
}

/// Marks the connection answered once its request has a response.
async fn answer(req: Request, next: Next) -> Response {
    let answered = req.extensions().get::<Answered>().cloned();
    let res = next.run(req).await;
    if let Some(a) = answered {
        a.keep();
    }
    res
}

/// An absolute-form request names its own authority: only the origin's.
async fn authority(State(gw): State<Gateway>, req: Request, next: Next) -> Response {
    if let Some(named) = req.uri().authority()
        && gw
            .origin()
            .is_none_or(|(o, _)| !named.as_str().eq_ignore_ascii_case(o.host()))
    {
        return not_found();
    }
    next.run(req).await
}

async fn harden(mut res: Response) -> Response {
    let h = res.headers_mut();
    for (name, value) in [
        (header::CONTENT_SECURITY_POLICY, CSP),
        (header::REFERRER_POLICY, "no-referrer"),
        (header::CACHE_CONTROL, "no-store"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::X_FRAME_OPTIONS, "DENY"),
        (
            header::HeaderName::from_static("x-robots-tag"),
            "noindex, nofollow",
        ),
        (
            header::HeaderName::from_static("cross-origin-opener-policy"),
            "same-origin",
        ),
        (
            header::HeaderName::from_static("cross-origin-resource-policy"),
            "same-origin",
        ),
    ] {
        h.insert(name, HeaderValue::from_static(value));
    }
    res
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "not found").into_response()
}

fn json(status: StatusCode, body: serde_json::Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

fn one(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    let mut all = headers.get_all(name).iter();
    let first = all.next()?;
    all.next().is_none().then(|| first.to_str().ok()).flatten()
}

/// The public origin and its generation, when the request names that origin
/// exactly as its `Host`.
fn host_ok(gw: &Gateway, headers: &HeaderMap) -> Option<(Origin, u64)> {
    let (origin, generation) = gw.origin()?;
    let host = one(headers, header::HOST)?;
    host.eq_ignore_ascii_case(origin.host())
        .then_some((origin, generation))
}

fn origin_ok(origin: &Origin, headers: &HeaderMap) -> bool {
    one(headers, header::ORIGIN).is_some_and(|o| o == origin.as_str())
}

/// Who is asking, for the failed-redemption limit: the address the tunnel
/// saw, or the loopback peer when no tunnel says.
fn client(headers: &HeaderMap) -> &str {
    one(headers, header::HeaderName::from_static("cf-connecting-ip")).unwrap_or("direct")
}

/// The value of this link's viewer cookie, if the request carries exactly one.
fn cookie(headers: &HeaderMap, id: &GrantId) -> Option<String> {
    let name = id.cookie_name();
    let mut found = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .filter(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string());
    let value = found.next()?;
    found.next().is_none().then_some(value)
}

async fn page(State(gw): State<Gateway>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    if host_ok(&gw, &headers).is_none() || GrantId::parse(&id).is_none() {
        return not_found();
    }
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], PAGE).into_response()
}

async fn asset(
    State(gw): State<Gateway>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    if host_ok(&gw, &headers).is_none() {
        return not_found();
    }
    match ASSETS.iter().find(|a| a.name == name) {
        Some(a) => ([(header::CONTENT_TYPE, a.kind)], a.body).into_response(),
        None => not_found(),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RedeemBody {
    secret: String,
}

fn refused(why: Refused) -> Response {
    let (status, code) = match why {
        Refused::NotFound => (StatusCode::NOT_FOUND, "not_found"),
        Refused::Gone => (StatusCode::GONE, "gone"),
        Refused::Full => (StatusCode::SERVICE_UNAVAILABLE, "full"),
        Refused::Throttled => (StatusCode::TOO_MANY_REQUESTS, "throttled"),
    };
    json(status, serde_json::json!({ "error": code }))
}

async fn redeem(
    State(gw): State<Gateway>,
    Path(id): Path<String>,
    headers: HeaderMap,
    conn: Option<axum::Extension<Answered>>,
    body: Bytes,
) -> Response {
    let Some((origin, generation)) = host_ok(&gw, &headers) else {
        return not_found();
    };
    let Some(id) = GrantId::parse(&id) else {
        return not_found();
    };
    if !origin_ok(&origin, &headers) {
        return json(
            StatusCode::FORBIDDEN,
            serde_json::json!({"error": "origin"}),
        );
    }
    if one(&headers, header::CONTENT_TYPE) != Some("application/json") {
        return json(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            serde_json::json!({"error": "json"}),
        );
    }
    let Ok(RedeemBody { secret }) = serde_json::from_slice(&body) else {
        return json(
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": "body"}),
        );
    };
    if !claim(conn.as_ref()) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match gw.redeem(client(&headers), generation, &id, &secret) {
        Ok(r) => {
            let now = gw.now();
            let set = format!(
                "{}={}; Max-Age={}; Path=/; Secure; HttpOnly; SameSite=Strict",
                id.cookie_name(),
                r.cookie,
                r.expires.cookie_max_age(now),
            );
            let mut res = json(
                StatusCode::OK,
                serde_json::json!({
                    "viewer": r.viewer.as_str(),
                    "expires_at": rfc3339(r.expires.wall),
                    "expires_in_ms": r.expires.remaining_ms(now),
                }),
            );
            // Base32, base64url, digits and ASCII punctuation: always a valid
            // header value.
            let set = HeaderValue::from_str(&set).expect("a cookie of header-safe bytes");
            res.headers_mut().insert(header::SET_COOKIE, set);
            res
        }
        Err(why) => refused(why),
    }
}

/// Whether this browser's credential may stream: for a page that cannot see
/// why its WebSocket failed.
async fn status(State(gw): State<Gateway>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    if host_ok(&gw, &headers).is_none() {
        return not_found();
    }
    let Some(id) = GrantId::parse(&id) else {
        return not_found();
    };
    gw.prune();
    let now = gw.now();
    let ok = cookie(&headers, &id).and_then(|c| gw.store().status(&id, &c, now));
    match ok {
        Some(expires) => json(
            StatusCode::OK,
            serde_json::json!({
                "state": "ok",
                "expires_at": rfc3339(expires.wall),
                "expires_in_ms": expires.remaining_ms(now),
            }),
        ),
        None => json(StatusCode::GONE, serde_json::json!({"state": "ended"})),
    }
}

async fn stream(
    State(gw): State<Gateway>,
    Path(id): Path<String>,
    headers: HeaderMap,
    conn: Option<axum::Extension<Answered>>,
    cut: Option<axum::Extension<Cut>>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let Some((origin, generation)) = host_ok(&gw, &headers) else {
        return not_found();
    };
    let Some(id) = GrantId::parse(&id) else {
        return not_found();
    };
    if !origin_ok(&origin, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(cookie) = cookie(&headers, &id) else {
        return StatusCode::FORBIDDEN.into_response();
    };
    if !claim(conn.as_ref()) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    gw.prune();
    // One stream per credential: a reconnect replaces the old stream once
    // its socket is gone, so the two never overlap, and takes over its slot.
    let mut carried = None;
    let admitted = loop {
        match gw.admit(generation, &id, &cookie) {
            Admit::Ready(a) => break a,
            Admit::Busy(old) => {
                // The slot comes to this request only while it waits: cut off
                // meanwhile, it drops `from`, and the slot is freed.
                let (to, mut from) = tokio::sync::oneshot::channel();
                old.replace(to);
                if tokio::time::timeout(TEARDOWN, old.done().cancelled())
                    .await
                    .is_err()
                {
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
                carried = from.try_recv().ok();
            }
            Admit::Refused(Refused::Full) => {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            Admit::Refused(Refused::Throttled) => {
                return StatusCode::TOO_MANY_REQUESTS.into_response();
            }
            Admit::Refused(_) => return StatusCode::FORBIDDEN.into_response(),
        }
    };
    let Some(slot) = carried.or_else(|| gw.inner.streams.clone().try_acquire_owned().ok()) else {
        // Admitted but never started: that costs the credential nothing.
        gw.store()
            .refund(&admitted.viewer, admitted.live.stream(), gw.now().mono);
        admitted.live.done().cancel();
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let guard = StreamGuard {
        gw: gw.clone(),
        viewer: admitted.viewer.clone(),
        live: admitted.live.clone(),
        slot: Some(slot),
    };
    // Cut off already: no upgrade, no byte.
    if admitted.live.stop().is_cancelled() {
        return StatusCode::GONE.into_response();
    }
    cut_on_stop(&admitted.live, cut.as_ref());
    upgrade
        .max_message_size(super::wire::MAX_INBOUND)
        .max_frame_size(super::wire::MAX_INBOUND)
        .write_buffer_size(WRITE_BUFFER)
        .max_write_buffer_size(MAX_WRITE_BUFFER)
        .on_upgrade(move |ws| super::stream::run(ws, gw, admitted, guard))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// At the cap the oldest connection still waiting on its own request
    /// makes way; one being answered never does, and with every one being
    /// answered the newcomer is refused.
    #[tokio::test]
    async fn only_an_unanswered_connection_makes_way() {
        let mut tasks = tokio::task::JoinSet::new();
        let mut conn = |answered: bool| {
            let a = Answered::default();
            if answered {
                assert!(a.keep());
            }
            Conn {
                handle: tasks.spawn(std::future::pending::<()>()),
                answered: a,
            }
        };
        let mut open: VecDeque<Conn> = (0..MAX_CONNECTIONS).map(|i| conn(i != 3)).collect();
        assert!(make_room(&mut open));
        assert_eq!(open.len(), MAX_CONNECTIONS - 1);
        assert!(open.iter().all(|c| c.answered.kept()));
        open.push_back(conn(true));
        assert!(!make_room(&mut open), "an answered connection was evicted");
        assert_eq!(open.len(), MAX_CONNECTIONS);
    }

    /// A request and the eviction of its connection race for one claim:
    /// whichever takes it first wins, so a request never commits on a
    /// connection being closed.
    #[test]
    fn a_request_and_its_eviction_cannot_both_win() {
        let kept = Answered::default();
        assert!(kept.keep());
        assert!(!kept.evict(), "evicted a connection a request had claimed");
        let evicted = Answered::default();
        assert!(evicted.evict());
        assert!(
            !evicted.keep(),
            "a request committed on an evicted connection"
        );
    }

    /// A stream cut off before its upgrade completes closes the connection
    /// HTTP still holds; one that finished first leaves it alone.
    #[tokio::test]
    async fn a_cutoff_before_the_upgrade_closes_its_connection() {
        let mut store = super::super::grants::Store::default();
        let now = super::super::grants::Now::current();
        let minted = store
            .create(
                super::super::CreateOpts::new(shux_core::model::SessionId::new()),
                now,
            )
            .unwrap();
        let cookie = store
            .redeem(&minted.id, &minted.secret, now)
            .unwrap()
            .cookie;
        let Admit::Ready(cut_off) = store.admit(&minted.id, &cookie, now) else {
            panic!("not admitted");
        };
        let cut = axum::Extension(Cut::default());
        cut_on_stop(&cut_off.live, Some(&cut));
        cut_off.live.end(super::super::EndReason::Revoked);
        shux_testkit::wait_until_async("the connection is cut", || {
            let c = cut.0.0.clone();
            async move { c.is_cancelled() }
        })
        .await;
        store.stream_ended(&cut_off.viewer, cut_off.live.stream(), now.mono);
        let Admit::Ready(finished) = store.admit(&minted.id, &cookie, now) else {
            panic!("not admitted");
        };
        let kept = axum::Extension(Cut::default());
        cut_on_stop(&finished.live, Some(&kept));
        finished.live.done().cancel();
        finished.live.end(super::super::EndReason::Revoked);
        tokio::task::yield_now().await;
        assert!(
            !kept.0.0.is_cancelled(),
            "a finished stream cut a connection"
        );
    }

    #[test]
    fn the_vendored_viewer_matches_its_checksums() {
        use sha2::{Digest as _, Sha256};
        let sums = include_str!("assets/ghostty-web-0.4.0/SHA256SUMS");
        let license = include_bytes!("assets/ghostty-web-0.4.0/LICENSE");
        let mut checked = 0;
        for line in sums.lines() {
            let (want, name) = line.split_once("  ").expect("a sha256sum line");
            let body: &[u8] = match name {
                "LICENSE" => license,
                _ => ASSETS.iter().find(|a| a.name == name).expect("served").body,
            };
            let got: String = Sha256::digest(body)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            assert_eq!(got, want, "{name} is not the vendored file");
            checked += 1;
        }
        assert_eq!(checked, 4, "every vendored file is listed");
    }

    #[test]
    fn a_cookie_is_this_links_and_only_one() {
        let id = GrantId::parse("abcdefgh").unwrap();
        let mut h = HeaderMap::new();
        h.append(
            header::COOKIE,
            HeaderValue::from_static("a=1; __Host-shux-v-abcdefgh=v.s"),
        );
        assert_eq!(cookie(&h, &id).as_deref(), Some("v.s"));
        h.append(
            header::COOKIE,
            HeaderValue::from_static("__Host-shux-v-abcdefgh=w.t"),
        );
        assert_eq!(cookie(&h, &id), None, "two cookies of one name: ambiguous");
        let other = GrantId::parse("hgfedcba").unwrap();
        assert_eq!(cookie(&h, &other), None);
    }

    #[test]
    fn origins_are_https_hosts_only() {
        assert_eq!(
            Origin::parse("https://Ab.trycloudflare.com")
                .unwrap()
                .host(),
            "ab.trycloudflare.com"
        );
        assert!(Origin::parse("https://localhost:8443").is_ok());
        for bad in [
            "http://x",
            "https://",
            "https://x/v",
            "https://x?y",
            "https://u@x",
            "x",
        ] {
            assert!(Origin::parse(bad).is_err(), "{bad}");
        }
    }
}
