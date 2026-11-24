//! The gateway's HTTP surface. Every route checks the exact public `Host`;
//! redemption and the WebSocket also check the exact `Origin`. Every response
//! carries the same strict headers, and none is cached.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::grants::{Admit, GrantId, Refused};
use super::{Gateway, Origin, StreamGuard, TEARDOWN, rfc3339};

/// HTTP connections open at once.
const MAX_CONNECTIONS: usize = 64;
const HEADER_READ: Duration = Duration::from_secs(10);
const MAX_BODY: usize = 512;

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

pub(super) async fn serve(
    listener: tokio::net::TcpListener,
    gw: Gateway,
    cancel: CancellationToken,
) {
    let app = router(gw);
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    let mut conns = tokio::task::JoinSet::new();
    loop {
        let accepted = tokio::select! {
            () = cancel.cancelled() => break,
            Some(_) = conns.join_next(), if !conns.is_empty() => continue,
            accepted = listener.accept() => accepted,
        };
        let Ok((tcp, _)) = accepted else { continue };
        // Over the cap, the connection is closed unread.
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            continue;
        };
        let service = hyper_util::service::TowerToHyperService::new(app.clone());
        let cancel = cancel.clone();
        conns.spawn(async move {
            let _slot = slot;
            let conn = hyper::server::conn::http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(HEADER_READ)
                .serve_connection(TokioIo::new(tcp), service)
                .with_upgrades();
            tokio::select! {
                () = cancel.cancelled() => {}
                _ = conn => {}
            }
        });
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
        .layer(axum::middleware::map_response(harden))
        .with_state(gw)
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

/// The public origin, when the request names it exactly as its `Host`.
fn host_ok(gw: &Gateway, headers: &HeaderMap) -> Option<Origin> {
    let origin = gw.origin()?;
    let host = one(headers, header::HOST)?;
    host.eq_ignore_ascii_case(origin.host()).then_some(origin)
}

fn origin_ok(origin: &Origin, headers: &HeaderMap) -> bool {
    one(headers, header::ORIGIN).is_some_and(|o| o == origin.as_str())
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

async fn redeem(
    State(gw): State<Gateway>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(origin) = host_ok(&gw, &headers) else {
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
    gw.prune();
    let redeemed = gw.store().redeem(&id, &secret, Instant::now());
    match redeemed {
        Ok(r) => {
            let max_age = r
                .expires
                .saturating_duration_since(Instant::now())
                .as_secs();
            let set = format!(
                "{}={}; Max-Age={max_age}; Path=/; Secure; HttpOnly; SameSite=Strict",
                id.cookie_name(),
                r.cookie
            );
            let mut res = json(
                StatusCode::OK,
                serde_json::json!({"viewer": r.viewer.as_str(), "expires_at": rfc3339(r.expires_wall)}),
            );
            match HeaderValue::from_str(&set) {
                Ok(v) => {
                    res.headers_mut().insert(header::SET_COOKIE, v);
                }
                Err(_) => return json(StatusCode::INTERNAL_SERVER_ERROR, serde_json::json!({})),
            }
            res
        }
        Err(Refused::NotFound) => json(
            StatusCode::NOT_FOUND,
            serde_json::json!({"error": "not_found"}),
        ),
        Err(Refused::Gone) => json(StatusCode::GONE, serde_json::json!({"error": "gone"})),
        Err(Refused::Full) => json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({"error": "full"}),
        ),
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
    let ok = cookie(&headers, &id).and_then(|c| gw.store().status(&id, &c, Instant::now()));
    match ok {
        Some((_, expires)) => json(
            StatusCode::OK,
            serde_json::json!({"state": "ok", "expires_at": rfc3339(expires)}),
        ),
        None => json(StatusCode::GONE, serde_json::json!({"state": "ended"})),
    }
}

async fn stream(
    State(gw): State<Gateway>,
    Path(id): Path<String>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let Some(origin) = host_ok(&gw, &headers) else {
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
    gw.prune();
    // One stream per credential: a reconnect replaces the old stream once
    // its socket is gone, so the two never overlap.
    let admitted = loop {
        let admit = gw.store().admit(&id, &cookie, Instant::now());
        match admit {
            Admit::Ready(a) => break a,
            Admit::Busy(old) => {
                old.end(super::EndReason::Replaced);
                if tokio::time::timeout(TEARDOWN, old.done().cancelled())
                    .await
                    .is_err()
                {
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
            }
            Admit::Refused(_) => return StatusCode::FORBIDDEN.into_response(),
        }
    };
    let release = |gw: &Gateway| {
        gw.store()
            .stream_ended(&admitted.viewer, admitted.live.stream());
        admitted.live.done().cancel();
    };
    let Ok(permit) = gw.inner.streams.clone().try_acquire_owned() else {
        release(&gw);
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let guard = StreamGuard {
        gw: gw.clone(),
        viewer: admitted.viewer.clone(),
        live: admitted.live.clone(),
        _permit: permit,
    };
    upgrade
        .max_message_size(super::wire::MAX_INBOUND)
        .max_frame_size(super::wire::MAX_INBOUND)
        .on_upgrade(move |ws| super::stream::run(ws, gw, admitted, guard))
}

#[cfg(test)]
mod tests {
    use super::*;

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
