//! JSON-RPC server — UDS and optional TCP listeners.
//!
//! Accepts connections, frames messages with LengthDelimitedCodec,
//! parses JSON-RPC 2.0 requests, dispatches to the router, and
//! sends responses back over the same connection.

use std::path::PathBuf;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use tokio::net::{TcpListener, UnixListener, UnixStream};
#[cfg(test)]
use tokio_util::codec::Framed;
use tracing::{debug, error, info, warn};

use crate::codec::{MAX_FRAME_SIZE, create_codec};
use crate::error::{ErrorCode, RpcError};
use crate::router::Router;

/// Server configuration.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Path to the Unix domain socket.
    pub socket_path: PathBuf,
    /// Optional TCP listen address (e.g., "127.0.0.1:9876").
    /// Empty string means TCP is disabled.
    pub tcp_addr: String,
    /// Auth token for TCP connections. Required when tcp_addr is set.
    pub auth_token: Option<String>,
}

/// The JSON-RPC server.
pub struct Server {
    config: ServerConfig,
    router: Router,
    cancel: tokio_util::sync::CancellationToken,
}

/// A JSON-RPC 2.0 request (parsed from incoming frames).
#[derive(Debug, serde::Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    #[serde(default)]
    id: Option<serde_json::Value>,
    method: String,
    #[serde(default)]
    params: Option<serde_json::Value>,
}

/// A JSON-RPC 2.0 response (serialized to outgoing frames).
#[derive(Debug, serde::Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

impl JsonRpcResponse {
    /// Create a success response.
    fn success(id: Option<serde_json::Value>, result: serde_json::Value) -> Self {
        JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Create an error response.
    fn error(id: Option<serde_json::Value>, error: RpcError) -> Self {
        JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(error),
        }
    }
}

impl Server {
    /// Create a new server with the given config and router.
    pub fn new(
        config: ServerConfig,
        router: Router,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Self {
        Server {
            config,
            router,
            cancel,
        }
    }

    /// Start the server (UDS + optional TCP).
    ///
    /// This runs until the CancellationToken is triggered.
    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        // Clean up any stale socket file.
        if self.config.socket_path.exists() {
            std::fs::remove_file(&self.config.socket_path)?;
        }

        // Ensure the parent directory exists.
        if let Some(parent) = self.config.socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Bind the Unix domain socket.
        let uds_listener = UnixListener::bind(&self.config.socket_path)?;
        info!(path = %self.config.socket_path.display(), "UDS listener bound");

        // Set socket permissions to 0700 (PRD §13.1).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o700);
            std::fs::set_permissions(&self.config.socket_path, perms)?;
            info!("socket permissions set to 0700");
        }

        // Optionally bind TCP listener.
        let tcp_listener = if !self.config.tcp_addr.is_empty() {
            let listener = TcpListener::bind(&self.config.tcp_addr).await?;
            info!(addr = %self.config.tcp_addr, "TCP listener bound");
            Some(listener)
        } else {
            None
        };

        let router = self.router.clone();
        let cancel = self.cancel.clone();
        let auth_token = self.config.auth_token.clone();

        // Accept loop.
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    info!("server shutting down");
                    break;
                }
                // Accept UDS connections.
                result = uds_listener.accept() => {
                    match result {
                        Ok((stream, _addr)) => {
                            debug!("accepted UDS connection");
                            let router = router.clone();
                            let cancel = cancel.clone();
                            tokio::spawn(async move {
                                if let Err(e) = handle_uds_connection(stream, router, cancel).await {
                                    warn!(error = %e, "UDS connection error");
                                }
                            });
                        }
                        Err(e) => {
                            error!(error = %e, "UDS accept error");
                        }
                    }
                }
                // Accept TCP connections (if enabled).
                result = async {
                    if let Some(ref listener) = tcp_listener {
                        listener.accept().await
                    } else {
                        // If TCP is disabled, park forever (select! picks UDS or cancel).
                        std::future::pending().await
                    }
                } => {
                    match result {
                        Ok((stream, addr)) => {
                            debug!(addr = %addr, "accepted TCP connection");
                            let router = router.clone();
                            let cancel = cancel.clone();
                            let token = auth_token.clone();
                            tokio::spawn(async move {
                                if let Err(e) = handle_tcp_connection(stream, router, cancel, token).await {
                                    warn!(error = %e, addr = %addr, "TCP connection error");
                                }
                            });
                        }
                        Err(e) => {
                            error!(error = %e, "TCP accept error");
                        }
                    }
                }
            }
        }

        // Cleanup socket file on shutdown.
        let _ = std::fs::remove_file(&self.config.socket_path);
        info!("server stopped");

        Ok(())
    }
}

/// Authentication requirement for a connection (PRD §13.1).
enum ConnAuth {
    /// UDS (socket-owning user) or TCP without a configured token.
    Trusted,
    /// TCP with a configured token: the first frame must authenticate.
    Pending(String),
}

/// Handle a UDS client connection.
///
/// UDS connections are trusted (no auth needed — PRD §13.1).
async fn handle_uds_connection(
    stream: UnixStream,
    router: Router,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Built from the concrete stream BEFORE the generic split — the monitor
    // needs the real socket fd for write-direction readiness.
    let monitor = PeerDeathMonitor::try_new(&stream);
    let (read_half, write_half) = stream.into_split();
    serve_connection(
        read_half,
        write_half,
        router,
        cancel,
        ConnAuth::Trusted,
        // Historical UDS behavior: attempt a frame_too_large error response
        // before closing on a codec error.
        true,
        monitor,
    )
    .await
}

/// Handle a TCP client connection.
///
/// TCP connections require auth token validation (PRD §13.1).
async fn handle_tcp_connection(
    stream: tokio::net::TcpStream,
    router: Router,
    cancel: tokio_util::sync::CancellationToken,
    expected_token: Option<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let monitor = PeerDeathMonitor::try_new(&stream);
    let (read_half, write_half) = stream.into_split();
    let auth = match expected_token {
        Some(token) => ConnAuth::Pending(token),
        None => ConnAuth::Trusted, // No token configured = no auth needed.
    };
    // Historical TCP behavior: close silently on codec errors.
    serve_connection(read_half, write_half, router, cancel, auth, false, monitor).await
}

/// Hard per-connection cap on frames queued awaiting execution (codex P3
/// round-2 major). The frame queue is UNBOUNDED so the read task's forward
/// NEVER blocks — a bounded queue let a client pipeline past capacity, park
/// the read task on `send()`, and starve EOF detection while the executor sat
/// in a long dispatch (the B2 waiter-leak through the over-pipelined path).
/// The cap re-bounds memory at the protocol level instead: exceeding it is a
/// protocol violation that deliberately cancels the WHOLE connection
/// (`conn_closed`), dropping the in-flight handler and closing the socket.
/// Worst-case memory stays `MAX_PENDING_FRAMES × MAX_FRAME_SIZE`.
///
/// Exposure honesty (codex P3 round-3 caveat): the cap counts PRE-AUTH TCP
/// frames too — an unauthenticated TCP peer can stage up to this many frames
/// before the executor's first-frame auth check severs it. That window is not
/// "already-trusted": it is bounded by this same cap (the executor dequeues
/// and auth-rejects frame #1 immediately, closing the connection), the TCP
/// listener is loopback + token-gated, and UDS callers are the socket-owning
/// user (0700). A separate tighter pre-auth counter would double the state
/// for no additional bound.
const MAX_PENDING_FRAMES: usize = 256;

/// Hard per-connection cap on BYTES queued awaiting execution (greptile PR #90
/// P1): the frame-count cap alone bounds nothing in bytes — 256 × 16 MiB
/// frames ≈ 4 GiB retained by one stalled connection. Enforced in the same
/// accounting as the frame cap (each queued frame adds its decoded length;
/// the executor subtracts on dequeue); exceeding it is the same deliberate
/// connection-cancel. Worst-case retention is this cap plus ONE frame (the
/// check runs after adding the frame that crossed it): 64 MiB + 16 MiB.
const MAX_PENDING_BYTES: usize = 64 * 1024 * 1024;

/// Detects PEER DEATH on the write half of a connection (codex-bot PR #90 P2:
/// read-side EOF must NOT be conflated with client death — a half-closed
/// client that did `shutdown(Write)` is still connected and waiting for its
/// responses).
///
/// Mechanism: the socket fd is dup'ed (shared file description, independent
/// readiness registration) and registered with `AsyncFd` for WRITABLE
/// interest. When the PEER closes its read side — full close, process death,
/// SIGKILL — the OS flags our WRITE direction: epoll reports EPOLLHUP/EPOLLERR
/// (Linux), kqueue sets EV_EOF on the EVFILT_WRITE filter (macOS). Tokio
/// surfaces both as `Ready::is_write_closed()` / `is_error()`. A mere
/// half-close (peer `shutdown(Write)`) flags only OUR read side, which this
/// monitor deliberately ignores.
///
/// No busy-spin: plain-writable wakeups call `clear_ready()`, which parks the
/// next `.ready()` until a NEW edge event — and the only later write-direction
/// transition on an idle connection is the peer-closure event itself.
///
/// Empirical verification on macOS (per the review's own caveat about
/// epoll/kqueue semantics): every existing full-close drop-guard test
/// (`test_disconnect_mid_request_drops_inflight_handler`, the backlog and
/// cross-connection variants, the in-process lens waiter-drop proof, and the
/// black-box SIGKILL test) now passes THROUGH this monitor — they fail with a
/// 5s timeout if kqueue does not deliver write-closed. The half-close tests
/// prove the inverse (no false positive).
struct PeerDeathMonitor {
    fd: tokio::io::unix::AsyncFd<std::os::fd::OwnedFd>,
}

impl PeerDeathMonitor {
    /// Dup the stream's fd and register it for write-direction readiness.
    /// `None` (dup/registration failure — effectively never) degrades the
    /// caller to cancel-on-EOF: B2 promptness is preserved, half-close
    /// support is lost for that one connection.
    fn try_new(stream: &impl std::os::fd::AsFd) -> Option<Self> {
        let owned = stream.as_fd().try_clone_to_owned().ok()?;
        let fd =
            tokio::io::unix::AsyncFd::with_interest(owned, tokio::io::Interest::WRITABLE).ok()?;
        Some(PeerDeathMonitor { fd })
    }

    /// Resolves when the peer is DEAD (write direction closed or errored).
    /// Never resolves for a live or merely half-closed peer.
    async fn peer_dead(&self) {
        loop {
            match self.fd.ready(tokio::io::Interest::WRITABLE).await {
                Ok(mut guard) => {
                    let ready = guard.ready();
                    if ready.is_write_closed() || ready.is_error() {
                        return;
                    }
                    // Plain writable: park until the next edge event.
                    guard.clear_ready();
                }
                // fd-level failure: treat as dead (conservative — matches
                // the pre-monitor behavior of closing on any read error).
                Err(_) => return,
            }
        }
    }
}

/// Serve one framed JSON-RPC connection with CANCELLABLE request execution.
///
/// Architecture (P3 codex B2 / claude major — client disconnect must drop
/// in-flight request futures; codex-bot PR #90 P2 — but a HALF-CLOSED client
/// is NOT a dead client): the connection is split so the READ side keeps
/// running while a request executes, and CLIENT DEATH is detected on the
/// WRITE direction, never inferred from read-side EOF.
///
/// - A spawned read task decodes frames and queues them on an UNBOUNDED
///   channel (bounded by `MAX_PENDING_FRAMES` / `MAX_PENDING_BYTES` at the
///   protocol level, above). Because it reads independently of request
///   execution AND its queue forward never blocks, the read task is ALWAYS
///   parked on the socket — EOF/read-error is reachable in every state,
///   regardless of executor progress or pipelining depth (codex P3 round-2:
///   a bounded queue broke exactly this).
/// - Read-side EOF does NOT cancel `conn_closed` (codex-bot PR #90 P2): a
///   client may `shutdown(Write)` after its last request and keep reading —
///   the executor drains the queued frames, finishes any in-flight request,
///   writes every response, and the connection ends naturally when `recv()`
///   returns `None`. `conn_closed` is cancelled only by: (1) the
///   `PeerDeathMonitor` observing the peer's FULL closure on the write
///   direction, (2) an IO-level read error (socket died mid-read), (3) the
///   pipelining caps (deliberate severing). Fallback: if the monitor could
///   not be built (`None` — dup failure, effectively never), EOF degrades to
///   cancel-on-EOF so B2 promptness is never lost, trading away half-close
///   support for that one connection.
/// - This executor drains the queue STRICTLY SERIALLY (response order equals
///   request order — exactly the pre-split semantics) and races both the
///   dequeue AND each dispatch against `conn_closed`: when the client DIES,
///   the in-flight handler FUTURE IS DROPPED — releasing whatever it held
///   (settle watch subscriptions, event long-polls, locks-on-await) — and
///   any still-queued frames are discarded (the accepted fire-and-forget
///   delta: a DEAD client's unprocessed requests are dropped; a half-closed
///   client's requests are all answered).
#[allow(clippy::too_many_arguments)]
async fn serve_connection<R, W>(
    read_half: R,
    write_half: W,
    router: Router,
    cancel: tokio_util::sync::CancellationToken,
    mut auth: ConnAuth,
    error_response_on_bad_frame: bool,
    monitor: Option<PeerDeathMonitor>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin,
{
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio_util::codec::{FramedRead, FramedWrite};

    let mut framed_tx = FramedWrite::new(write_half, create_codec());
    let (frame_tx, mut frame_rx) =
        tokio::sync::mpsc::unbounded_channel::<Result<bytes::BytesMut, std::io::Error>>();
    let conn_closed = tokio_util::sync::CancellationToken::new();
    // Frames/bytes queued but not yet dequeued by the executor. Written by
    // the read task (inc) and this executor (dec); the tiny inc-before-send
    // window can only OVER-count, so the caps can never be evaded.
    let pending_frames = Arc::new(AtomicUsize::new(0));
    let pending_bytes = Arc::new(AtomicUsize::new(0));

    // Client-death watcher (codex-bot PR #90 P2): cancels `conn_closed` when
    // the peer FULLY closes — the only signal that means "nobody can receive
    // responses anymore". Runs from connection start, so a SIGKILLed client
    // is detected promptly whether or not the read task has seen EOF yet.
    let monitor_task = monitor.map(|m| {
        let monitor_closed = conn_closed.clone();
        tokio::spawn(async move {
            m.peer_dead().await;
            debug!("peer fully closed; cancelling connection");
            monitor_closed.cancel();
        })
    });
    let have_monitor = monitor_task.is_some();

    let read_closed = conn_closed.clone();
    let read_pending = pending_frames.clone();
    let read_bytes = pending_bytes.clone();
    let read_task = tokio::spawn(async move {
        let mut framed_rx = FramedRead::new(read_half, create_codec());
        // Which exits CANCEL the connection: IO-level read errors and cap
        // violations do; clean EOF (half-close — codex-bot PR #90 P2) and
        // decode errors (deterministic frame_too_large response — codex P3
        // round-3) do NOT. Client death in the latter states is covered by
        // the PeerDeathMonitor (or by cancel-on-EOF in the monitor-less
        // fallback).
        let mut cancel_on_exit = false;
        loop {
            match framed_rx.next().await {
                Some(Ok(frame)) => {
                    let queued = read_pending.fetch_add(1, Ordering::SeqCst) + 1;
                    let queued_bytes =
                        read_bytes.fetch_add(frame.len(), Ordering::SeqCst) + frame.len();
                    if queued > MAX_PENDING_FRAMES || queued_bytes > MAX_PENDING_BYTES {
                        // Protocol violation: deliberate, observable
                        // connection cancellation (the client sees EOF).
                        warn!(
                            queued,
                            queued_bytes,
                            frame_cap = MAX_PENDING_FRAMES,
                            byte_cap = MAX_PENDING_BYTES,
                            "pipelining cap exceeded; cancelling connection"
                        );
                        cancel_on_exit = true;
                        break;
                    }
                    // UnboundedSender::send is non-blocking by construction:
                    // the read task returns to the socket immediately, so EOF
                    // detection can never be starved by a full queue.
                    if frame_tx.send(Ok(frame)).is_err() {
                        break; // Executor gone; stop reading.
                    }
                }
                Some(Err(e)) => {
                    // LengthDelimitedCodec signals an oversized/invalid frame
                    // as io::ErrorKind::InvalidData (verified against
                    // tokio-util 0.7's length_delimited.rs and pinned by
                    // codec::tests::test_codec_max_frame_size); every other
                    // kind is an IO-level read error, i.e. the socket died.
                    // Decode errors must NOT cancel: the executor dequeues
                    // the Err SERIALLY and answers it deterministically
                    // (codex P3 round-3); post-error client death is the
                    // monitor's job.
                    cancel_on_exit = e.kind() != std::io::ErrorKind::InvalidData;
                    let _ = frame_tx.send(Err(e));
                    break;
                }
                None => {
                    // Clean EOF: the client closed its WRITE side. With a
                    // monitor, this is possibly just a half-close — keep the
                    // connection alive so every queued/in-flight request is
                    // answered (codex-bot PR #90 P2). Without one, degrade
                    // to the old cancel-on-EOF (B2 over half-close).
                    cancel_on_exit = !have_monitor;
                    break;
                }
            }
        }
        // The dropped frame_tx lets the executor drain to None and finish.
        if cancel_on_exit {
            read_closed.cancel();
        }
    });

    // Executor loop. Errors (`?` on writes) must still abort the read task,
    // so run the loop in an inner async block and join on `result` below.
    let result: Result<(), Box<dyn std::error::Error + Send + Sync>> = async {
        loop {
            let maybe_frame = tokio::select! {
                _ = cancel.cancelled() => break,
                // Client DEAD (peer-death monitor / IO read error) or cap
                // violation: stop deterministically instead of draining a
                // dead client's queued frames (accepted fire-and-forget
                // delta). Never fires for a merely half-closed client, whose
                // queued requests are all drained and answered below.
                _ = conn_closed.cancelled() => {
                    debug!("connection closed; discarding queued frames");
                    break;
                }
                m = frame_rx.recv() => m,
            };
            let data = match maybe_frame {
                None => {
                    // Read side finished (EOF or half-close) and every queued
                    // frame has been drained and answered.
                    debug!("client disconnected");
                    break;
                }
                Some(Err(e)) => {
                    // Frame too large or codec error.
                    warn!(error = %e, "frame error, closing connection");
                    if error_response_on_bad_frame {
                        let error_response = JsonRpcResponse::error(
                            None,
                            RpcError::frame_too_large(0, MAX_FRAME_SIZE),
                        );
                        if let Ok(bytes) = serde_json::to_vec(&error_response) {
                            let _ = framed_tx.send(Bytes::from(bytes)).await;
                        }
                    }
                    break;
                }
                Some(Ok(data)) => {
                    pending_frames.fetch_sub(1, Ordering::SeqCst);
                    pending_bytes.fetch_sub(data.len(), Ordering::SeqCst);
                    data
                }
            };

            // If not yet authenticated, the first message must be an auth
            // request (unchanged TCP-auth semantics).
            if let ConnAuth::Pending(expected) = &auth {
                let expected = Some(expected.clone());
                match try_authenticate(&data, &expected) {
                    Ok(true) => {
                        auth = ConnAuth::Trusted;
                        let response = JsonRpcResponse::success(
                            Some(serde_json::json!(0)),
                            serde_json::json!({"authenticated": true}),
                        );
                        let bytes = serde_json::to_vec(&response)?;
                        framed_tx.send(Bytes::from(bytes)).await?;
                        continue;
                    }
                    Ok(false) | Err(_) => {
                        let error_response =
                            JsonRpcResponse::error(None, RpcError::new(ErrorCode::AuthRequired));
                        let bytes = serde_json::to_vec(&error_response)?;
                        framed_tx.send(Bytes::from(bytes)).await?;
                        break; // Close connection on auth failure.
                    }
                }
            }

            // The B2 fix: the dispatch races the connection-close signal, so
            // a client that disconnects mid-request DROPS the handler future
            // instead of letting it run to completion unobserved.
            let response = tokio::select! {
                _ = conn_closed.cancelled() => {
                    debug!("client disconnected mid-request; dropping in-flight handler");
                    break;
                }
                response = process_frame(&data, &router) => response,
            };
            let response_bytes = serde_json::to_vec(&response)?;
            framed_tx.send(Bytes::from(response_bytes)).await?;
        }
        Ok(())
    }
    .await;

    // Stop the read + monitor tasks on every exit path (server shutdown,
    // write error, auth failure) — otherwise they would idle on a still-open
    // socket forever.
    read_task.abort();
    let _ = read_task.await;
    if let Some(task) = monitor_task {
        task.abort();
        let _ = task.await;
    }
    result
}

/// Process a single frame: parse JSON-RPC, dispatch, return response.
async fn process_frame(data: &[u8], router: &Router) -> JsonRpcResponse {
    // Parse JSON.
    let request: JsonRpcRequest = match serde_json::from_slice(data) {
        Ok(req) => req,
        Err(e) => {
            warn!(error = %e, "failed to parse JSON-RPC request");
            return JsonRpcResponse::error(
                None,
                RpcError::with_message(ErrorCode::ParseError, format!("JSON parse error: {e}")),
            );
        }
    };

    // Validate JSON-RPC version.
    if request.jsonrpc != "2.0" {
        return JsonRpcResponse::error(
            request.id,
            RpcError::with_data(
                ErrorCode::InvalidRequest,
                serde_json::json!({
                    "detail": "jsonrpc field must be \"2.0\"",
                    "got": request.jsonrpc
                }),
            ),
        );
    }

    // Dispatch to router.
    match router.dispatch(&request.method, request.params).await {
        Ok(result) => JsonRpcResponse::success(request.id, result),
        Err(error) => JsonRpcResponse::error(request.id, error),
    }
}

/// Try to authenticate a TCP connection.
///
/// Expects a JSON-RPC request with method "auth" and params.token matching
/// the expected token.
fn try_authenticate(
    data: &[u8],
    expected_token: &Option<String>,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    // No token configured = no auth needed, always pass.
    let Some(expected) = expected_token else {
        return Ok(true);
    };
    let request: JsonRpcRequest = serde_json::from_slice(data)?;
    if request.method != "auth" {
        return Ok(false);
    }
    let Some(params) = request.params else {
        return Ok(false);
    };
    let token = params.get("token").and_then(|v| v.as_str());
    Ok(token == Some(expected.as_str()))
}

/// Register the system-level built-in methods on a router builder.
///
/// System methods (always available, no state dependency):
/// - system.version -- returns version info
/// - system.health -- returns health status
///
/// Session methods (session.create, session.list, session.kill, session.ensure)
/// are registered by the binary crate since they require a GraphHandle which
/// lives in shux-core (and shux-rpc intentionally does not depend on shux-core).
pub fn register_builtin_methods(
    builder: crate::router::RouterBuilder,
) -> crate::router::RouterBuilder {
    use crate::policy::{Policy, Sensitivity};
    builder
        .register_with_policy(
            "system.version",
            Policy::fixed(Sensitivity::Public),
            |_params: Option<serde_json::Value>| async {
                Ok(serde_json::json!({
                    "version": env!("CARGO_PKG_VERSION"),
                    "git_sha": env!("SHUX_GIT_SHA"),
                    "name": "shux",
                }))
            },
        )
        .register_with_policy(
            "system.health",
            Policy::fixed(Sensitivity::Public),
            |_params: Option<serde_json::Value>| async {
                Ok(serde_json::json!({
                    "status": "ok",
                    "uptime_secs": 0, // TODO: track actual uptime (task 001)
                }))
            },
        )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[tokio::test]
    async fn test_process_frame_valid_request() {
        let router = Router::builder()
            .register("system.version", |_: Option<serde_json::Value>| async {
                Ok(serde_json::json!({"version": "0.1.0"}))
            })
            .build();

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "system.version"
        });
        let data = serde_json::to_vec(&request).unwrap();

        let response = process_frame(&data, &router).await;
        assert!(response.error.is_none());
        assert_eq!(response.result.unwrap()["version"], "0.1.0");
        assert_eq!(response.id, Some(serde_json::json!(1)));
    }

    #[tokio::test]
    async fn test_process_frame_method_not_found() {
        let router = Router::builder().build();

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "nonexistent"
        });
        let data = serde_json::to_vec(&request).unwrap();

        let response = process_frame(&data, &router).await;
        assert!(response.error.is_some());
        assert_eq!(response.error.unwrap().code, -32601);
    }

    #[tokio::test]
    async fn test_process_frame_invalid_json() {
        let router = Router::builder().build();
        let data = b"not valid json at all";

        let response = process_frame(data, &router).await;
        assert!(response.error.is_some());
        assert_eq!(response.error.unwrap().code, -32700);
    }

    #[tokio::test]
    async fn test_process_frame_wrong_version() {
        let router = Router::builder().build();

        let request = serde_json::json!({
            "jsonrpc": "1.0",
            "id": 3,
            "method": "system.version"
        });
        let data = serde_json::to_vec(&request).unwrap();

        let response = process_frame(data.as_slice(), &router).await;
        assert!(response.error.is_some());
        assert_eq!(response.error.unwrap().code, -32600);
    }

    #[tokio::test]
    async fn test_builtin_methods() {
        let router = register_builtin_methods(Router::builder()).build();

        assert!(router.has_method("system.version"));
        assert!(router.has_method("system.health"));

        let result = router.dispatch("system.version", None).await.unwrap();
        assert_eq!(result["name"], "shux");

        let result = router.dispatch("system.health", None).await.unwrap();
        assert_eq!(result["status"], "ok");
    }

    #[tokio::test]
    async fn test_try_authenticate() {
        let token = Some("secret123".to_string());

        // Valid auth.
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "auth",
            "params": {"token": "secret123"}
        });
        let data = serde_json::to_vec(&request).unwrap();
        assert!(try_authenticate(&data, &token).unwrap());

        // Invalid token.
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "auth",
            "params": {"token": "wrong"}
        });
        let data = serde_json::to_vec(&request).unwrap();
        assert!(!try_authenticate(&data, &token).unwrap());

        // Wrong method.
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "system.version"
        });
        let data = serde_json::to_vec(&request).unwrap();
        assert!(!try_authenticate(&data, &token).unwrap());

        // No token configured = always authenticated.
        let no_token: Option<String> = None;
        assert!(try_authenticate(&data, &no_token).unwrap());
    }

    #[tokio::test]
    async fn test_uds_server_integration() {
        // Integration test: start a server, connect a client, send a request.
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("test.sock");

        let cancel = tokio_util::sync::CancellationToken::new();
        let router = register_builtin_methods(Router::builder()).build();
        let config = ServerConfig {
            socket_path: socket_path.clone(),
            tcp_addr: String::new(),
            auth_token: None,
        };

        let server = Server::new(config, router, cancel.clone());

        // Run server in background.
        let server_handle = tokio::spawn(async move {
            server.run().await.unwrap();
        });

        // Give the server a moment to bind.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Connect a client.
        let stream = UnixStream::connect(&socket_path).await.unwrap();
        let mut framed = Framed::new(stream, create_codec());

        // Send a request.
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "system.version"
        });
        let request_bytes = serde_json::to_vec(&request).unwrap();
        framed.send(Bytes::from(request_bytes)).await.unwrap();

        // Read the response.
        let response_frame = framed.next().await.unwrap().unwrap();
        let response: serde_json::Value = serde_json::from_slice(&response_frame).unwrap();

        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 1);
        assert!(response["result"]["version"].is_string());
        assert_eq!(response["result"]["name"], "shux");

        // Shutdown.
        cancel.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_handle).await;
    }

    /// Connect to a UDS server with a bounded retry loop (replaces a fixed
    /// bind sleep with a deadline-bounded event wait).
    async fn connect_with_retries(path: &std::path::Path) -> UnixStream {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match UnixStream::connect(path).await {
                Ok(s) => return s,
                Err(e) => {
                    if tokio::time::Instant::now() >= deadline {
                        panic!("server never bound {}: {e}", path.display());
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            }
        }
    }

    /// Drop guard for the hang handler: proves the in-flight future was
    /// DROPPED (aborted) rather than run to completion. `notify_one` queues a
    /// permit, so the test cannot miss the signal even if it subscribes late.
    struct HangGuard {
        dropped: Arc<std::sync::atomic::AtomicBool>,
        notify: Arc<tokio::sync::Notify>,
    }

    impl Drop for HangGuard {
        fn drop(&mut self) {
            self.dropped
                .store(true, std::sync::atomic::Ordering::SeqCst);
            self.notify.notify_one();
        }
    }

    /// Router with a `test.hang` method that signals entry, then pends
    /// forever holding a HangGuard. Returns (router, entered, dropped,
    /// dropped_notify).
    #[allow(clippy::type_complexity)]
    fn hang_router() -> (
        Router,
        Arc<tokio::sync::Notify>,
        Arc<std::sync::atomic::AtomicBool>,
        Arc<tokio::sync::Notify>,
    ) {
        use std::sync::atomic::AtomicBool;
        let entered = Arc::new(tokio::sync::Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let dropped_notify = Arc::new(tokio::sync::Notify::new());
        let entered_h = entered.clone();
        let dropped_h = dropped.clone();
        let dropped_notify_h = dropped_notify.clone();
        let router = register_builtin_methods(Router::builder())
            .register("test.hang", move |_: Option<serde_json::Value>| {
                let entered = entered_h.clone();
                let guard = HangGuard {
                    dropped: dropped_h.clone(),
                    notify: dropped_notify_h.clone(),
                };
                async move {
                    entered.notify_one();
                    let _guard = guard;
                    std::future::pending::<()>().await;
                    unreachable!("test.hang never completes")
                }
            })
            .build();
        (router, entered, dropped, dropped_notify)
    }

    fn frame_bytes(value: &serde_json::Value) -> Bytes {
        Bytes::from(serde_json::to_vec(value).unwrap())
    }

    #[tokio::test]
    async fn test_disconnect_mid_request_drops_inflight_handler() {
        // P3 codex B2: a client disconnecting while its request is executing
        // must ABORT the handler future (observable via the drop guard) —
        // previously the connection task awaited process_frame inline, so an
        // abandoned long-running request lived until it completed on its own.
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("cancel.sock");
        let cancel = tokio_util::sync::CancellationToken::new();
        let (router, entered, dropped, dropped_notify) = hang_router();

        let server = Server::new(
            ServerConfig {
                socket_path: socket_path.clone(),
                tcp_addr: String::new(),
                auth_token: None,
            },
            router,
            cancel.clone(),
        );
        let server_handle = tokio::spawn(async move {
            server.run().await.unwrap();
        });

        let stream = connect_with_retries(&socket_path).await;
        let mut framed = Framed::new(stream, create_codec());
        framed
            .send(frame_bytes(&serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": "test.hang"
            })))
            .await
            .unwrap();

        // Deterministic: the handler signals when it is actually executing.
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .expect("test.hang handler never entered");
        assert!(
            !dropped.load(std::sync::atomic::Ordering::SeqCst),
            "guard must still be live while the client is connected"
        );

        // Client disconnect (the CLI-SIGKILL equivalent at socket level).
        drop(framed);

        // The in-flight future must be dropped promptly — not after the
        // (infinite) handler completes.
        tokio::time::timeout(std::time::Duration::from_secs(5), dropped_notify.notified())
            .await
            .expect("in-flight handler was not dropped on client disconnect");
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));

        // Server stays healthy: a fresh connection serves normal requests.
        let stream2 = connect_with_retries(&socket_path).await;
        let mut framed2 = Framed::new(stream2, create_codec());
        framed2
            .send(frame_bytes(&serde_json::json!({
                "jsonrpc": "2.0", "id": 2, "method": "system.health"
            })))
            .await
            .unwrap();
        let response: serde_json::Value =
            serde_json::from_slice(&framed2.next().await.unwrap().unwrap()).unwrap();
        assert_eq!(response["result"]["status"], "ok");

        cancel.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_handle).await;
    }

    #[tokio::test]
    async fn test_sequential_requests_unaffected_by_split_execution() {
        // Post-refactor sanity: requests on one connection stay strictly
        // serial and ordered (response i pairs with request i, ids intact).
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("serial.sock");
        let cancel = tokio_util::sync::CancellationToken::new();
        let router = register_builtin_methods(Router::builder()).build();
        let server = Server::new(
            ServerConfig {
                socket_path: socket_path.clone(),
                tcp_addr: String::new(),
                auth_token: None,
            },
            router,
            cancel.clone(),
        );
        let server_handle = tokio::spawn(async move {
            server.run().await.unwrap();
        });

        let stream = connect_with_retries(&socket_path).await;
        let mut framed = Framed::new(stream, create_codec());

        // Pipeline both requests up front, then read both responses.
        framed
            .send(frame_bytes(&serde_json::json!({
                "jsonrpc": "2.0", "id": 10, "method": "system.version"
            })))
            .await
            .unwrap();
        framed
            .send(frame_bytes(&serde_json::json!({
                "jsonrpc": "2.0", "id": 11, "method": "system.health"
            })))
            .await
            .unwrap();

        let first: serde_json::Value =
            serde_json::from_slice(&framed.next().await.unwrap().unwrap()).unwrap();
        assert_eq!(first["id"], 10);
        assert_eq!(first["result"]["name"], "shux");
        let second: serde_json::Value =
            serde_json::from_slice(&framed.next().await.unwrap().unwrap()).unwrap();
        assert_eq!(second["id"], 11);
        assert_eq!(second["result"]["status"], "ok");

        cancel.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_handle).await;
    }

    #[tokio::test]
    async fn test_disconnect_does_not_affect_other_connections() {
        // Connection-scoped cancellation: dropping conn A (mid-hang) must not
        // disturb conn B's request flow, before or after the drop.
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("isolate.sock");
        let cancel = tokio_util::sync::CancellationToken::new();
        let (router, entered, dropped, dropped_notify) = hang_router();
        let server = Server::new(
            ServerConfig {
                socket_path: socket_path.clone(),
                tcp_addr: String::new(),
                auth_token: None,
            },
            router,
            cancel.clone(),
        );
        let server_handle = tokio::spawn(async move {
            server.run().await.unwrap();
        });

        // Conn A: park a hanging request.
        let stream_a = connect_with_retries(&socket_path).await;
        let mut framed_a = Framed::new(stream_a, create_codec());
        framed_a
            .send(frame_bytes(&serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": "test.hang"
            })))
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .expect("hang handler never entered");

        // Conn B: normal request completes WHILE A's request is in flight.
        let stream_b = connect_with_retries(&socket_path).await;
        let mut framed_b = Framed::new(stream_b, create_codec());
        framed_b
            .send(frame_bytes(&serde_json::json!({
                "jsonrpc": "2.0", "id": 2, "method": "system.health"
            })))
            .await
            .unwrap();
        let response: serde_json::Value =
            serde_json::from_slice(&framed_b.next().await.unwrap().unwrap()).unwrap();
        assert_eq!(response["result"]["status"], "ok");

        // Drop A → its in-flight handler is aborted...
        drop(framed_a);
        tokio::time::timeout(std::time::Duration::from_secs(5), dropped_notify.notified())
            .await
            .expect("conn A's in-flight handler was not dropped");
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));

        // ...and B keeps working, unaffected.
        framed_b
            .send(frame_bytes(&serde_json::json!({
                "jsonrpc": "2.0", "id": 3, "method": "system.version"
            })))
            .await
            .unwrap();
        let response: serde_json::Value =
            serde_json::from_slice(&framed_b.next().await.unwrap().unwrap()).unwrap();
        assert_eq!(response["id"], 3);
        assert_eq!(response["result"]["name"], "shux");

        cancel.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_handle).await;
    }

    #[tokio::test]
    async fn test_pipelined_backlog_disconnect_still_drops_inflight_handler() {
        // codex P3 round-2 major, exact scenario: a long-running request plus
        // 9+ PIPELINED frames on the same connection, then disconnect. With
        // the old bounded(8) queue the read task blocked on send() before it
        // could observe EOF, so conn_closed never fired and the in-flight
        // handler lived until completion — this test deadlocked at the
        // dropped_notify timeout. The unbounded queue keeps the read task
        // parked on the SOCKET in every state, so EOF is always reachable.
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("backlog.sock");
        let cancel = tokio_util::sync::CancellationToken::new();
        let (router, entered, dropped, dropped_notify) = hang_router();
        let server = Server::new(
            ServerConfig {
                socket_path: socket_path.clone(),
                tcp_addr: String::new(),
                auth_token: None,
            },
            router,
            cancel.clone(),
        );
        let server_handle = tokio::spawn(async move {
            server.run().await.unwrap();
        });

        let stream = connect_with_retries(&socket_path).await;
        let mut framed = Framed::new(stream, create_codec());
        framed
            .send(frame_bytes(&serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": "test.hang"
            })))
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .expect("test.hang handler never entered");

        // Pipeline 12 more frames while the executor is stuck in the hang —
        // past the old bounded(8) capacity, so the old code jams right here.
        for id in 2..14 {
            framed
                .send(frame_bytes(&serde_json::json!({
                    "jsonrpc": "2.0", "id": id, "method": "system.version"
                })))
                .await
                .unwrap();
        }

        // Disconnect with the backlog still queued.
        drop(framed);

        // The in-flight handler must still be dropped promptly.
        tokio::time::timeout(std::time::Duration::from_secs(5), dropped_notify.notified())
            .await
            .expect("in-flight handler not dropped under a pipelined backlog");
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));

        cancel.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_handle).await;
    }

    #[tokio::test]
    async fn test_pipelining_cap_exceeded_cancels_connection() {
        // MAX_PENDING_FRAMES is a protocol cap: a client that floods more
        // pending frames than the cap (while the executor is busy) has its
        // WHOLE connection deliberately cancelled — the in-flight handler is
        // dropped and the client observes EOF, even though the client never
        // closed its side.
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("cap.sock");
        let cancel = tokio_util::sync::CancellationToken::new();
        let (router, entered, dropped, dropped_notify) = hang_router();
        let server = Server::new(
            ServerConfig {
                socket_path: socket_path.clone(),
                tcp_addr: String::new(),
                auth_token: None,
            },
            router,
            cancel.clone(),
        );
        let server_handle = tokio::spawn(async move {
            server.run().await.unwrap();
        });

        let stream = connect_with_retries(&socket_path).await;
        let mut framed = Framed::new(stream, create_codec());
        framed
            .send(frame_bytes(&serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": "test.hang"
            })))
            .await
            .unwrap();
        // Ensure the hang frame has been DEQUEUED (the handler is running)
        // before flooding, so the pending count below is purely the flood.
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .expect("test.hang handler never entered");

        // Flood past the cap while the executor is pinned in the hang. The
        // server may sever the connection MID-flood (that severing is the
        // behavior under test), so a send error (broken pipe / reset) here is
        // acceptable — the flood simply stops.
        for id in 0..(MAX_PENDING_FRAMES + 44) {
            if framed
                .send(frame_bytes(&serde_json::json!({
                    "jsonrpc": "2.0", "id": id, "method": "system.version"
                })))
                .await
                .is_err()
            {
                break; // Server already closed the violating connection.
            }
        }

        // Deliberate cancellation: the in-flight handler is dropped...
        tokio::time::timeout(std::time::Duration::from_secs(5), dropped_notify.notified())
            .await
            .expect("cap violation did not drop the in-flight handler");
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));

        // ...and the CLIENT (which never closed) observes the connection
        // being closed by the server: EOF or a connection error — never a
        // response frame, and never a hang.
        let next = tokio::time::timeout(std::time::Duration::from_secs(5), framed.next())
            .await
            .expect("server did not close the violating connection");
        assert!(
            !matches!(next, Some(Ok(_))),
            "expected EOF/error after the cap violation, got a frame: {next:?}"
        );

        cancel.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_handle).await;
    }

    #[tokio::test]
    async fn test_half_close_still_receives_response() {
        // codex-bot PR #90 P2: client pattern `write request → shutdown(Write)
        // → read response`. Read-side EOF must NOT be treated as client death
        // — the half-closed client is still connected for responses. (The
        // round-2/3 code cancelled on EOF and discarded the queued frame, so
        // the client got bare EOF.)
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("halfclose.sock");
        let cancel = tokio_util::sync::CancellationToken::new();
        let router = register_builtin_methods(Router::builder()).build();
        let server = Server::new(
            ServerConfig {
                socket_path: socket_path.clone(),
                tcp_addr: String::new(),
                auth_token: None,
            },
            router,
            cancel.clone(),
        );
        let server_handle = tokio::spawn(async move {
            server.run().await.unwrap();
        });

        // Run several iterations: the old bug was a race (EOF vs dequeue), so
        // a single pass could false-green.
        for iteration in 0..10 {
            let stream = connect_with_retries(&socket_path).await;
            let (read_half, write_half) = stream.into_split();
            let mut framed_w = tokio_util::codec::FramedWrite::new(write_half, create_codec());
            let mut framed_r = tokio_util::codec::FramedRead::new(read_half, create_codec());

            framed_w
                .send(frame_bytes(&serde_json::json!({
                    "jsonrpc": "2.0", "id": 1, "method": "system.version"
                })))
                .await
                .unwrap();
            // Half-close: dropping the write half sends FIN (tokio's
            // OwnedWriteHalf shuts down the write direction on drop). The
            // read half stays open for the response.
            drop(framed_w);

            let response_frame =
                tokio::time::timeout(std::time::Duration::from_secs(5), framed_r.next())
                    .await
                    .unwrap_or_else(|_| {
                        panic!("iteration {iteration}: no response after half-close")
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "iteration {iteration}: bare EOF — the half-closed \
                             client's request was discarded"
                        )
                    })
                    .expect("decode response");
            let response: serde_json::Value = serde_json::from_slice(&response_frame).unwrap();
            assert_eq!(response["id"], 1, "iteration {iteration}: {response}");
            assert_eq!(response["result"]["name"], "shux");

            // Then the connection ends naturally.
            let next = tokio::time::timeout(std::time::Duration::from_secs(5), framed_r.next())
                .await
                .expect("connection should close after the drain");
            assert!(!matches!(next, Some(Ok(_))));
        }

        cancel.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_handle).await;
    }

    #[tokio::test]
    async fn test_half_close_drains_backlog_behind_inflight_request() {
        // codex-bot PR #90 P2, hard variant: the half-close lands while a
        // SLOW request is IN FLIGHT with another request queued behind it.
        // The executor must finish the in-flight request, drain the queued
        // one, and deliver BOTH responses in order — read-side EOF must not
        // abort either.
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let entered_h = entered.clone();
        let release_h = release.clone();
        let router = register_builtin_methods(Router::builder())
            .register("test.gate", move |_: Option<serde_json::Value>| {
                let entered = entered_h.clone();
                let release = release_h.clone();
                async move {
                    entered.notify_one();
                    release.notified().await;
                    Ok(serde_json::json!({"gate": "released"}))
                }
            })
            .build();

        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("halfclose-gate.sock");
        let cancel = tokio_util::sync::CancellationToken::new();
        let server = Server::new(
            ServerConfig {
                socket_path: socket_path.clone(),
                tcp_addr: String::new(),
                auth_token: None,
            },
            router,
            cancel.clone(),
        );
        let server_handle = tokio::spawn(async move {
            server.run().await.unwrap();
        });

        let stream = connect_with_retries(&socket_path).await;
        let (read_half, write_half) = stream.into_split();
        let mut framed_w = tokio_util::codec::FramedWrite::new(write_half, create_codec());
        let mut framed_r = tokio_util::codec::FramedRead::new(read_half, create_codec());

        // Slow request enters execution...
        framed_w
            .send(frame_bytes(&serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": "test.gate"
            })))
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .expect("gate handler never entered");
        // ...a second request queues behind it, then the client half-closes
        // while the gate is still held.
        framed_w
            .send(frame_bytes(&serde_json::json!({
                "jsonrpc": "2.0", "id": 2, "method": "system.health"
            })))
            .await
            .unwrap();
        drop(framed_w); // shutdown(Write): EOF reaches the server NOW.

        // Release the gate only after the half-close is in flight.
        release.notify_one();

        // Both responses must arrive, in order.
        let first: serde_json::Value = serde_json::from_slice(
            &tokio::time::timeout(std::time::Duration::from_secs(5), framed_r.next())
                .await
                .expect("gate response after half-close")
                .expect("not bare EOF — in-flight request was dropped")
                .expect("decode"),
        )
        .unwrap();
        assert_eq!(first["id"], 1);
        assert_eq!(first["result"]["gate"], "released");

        let second: serde_json::Value = serde_json::from_slice(
            &tokio::time::timeout(std::time::Duration::from_secs(5), framed_r.next())
                .await
                .expect("queued response after half-close")
                .expect("not bare EOF — queued frame was discarded")
                .expect("decode"),
        )
        .unwrap();
        assert_eq!(second["id"], 2);
        assert_eq!(second["result"]["status"], "ok");

        // Then natural close.
        let next = tokio::time::timeout(std::time::Duration::from_secs(5), framed_r.next())
            .await
            .expect("connection should close after the drain");
        assert!(!matches!(next, Some(Ok(_))));

        cancel.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_handle).await;
    }

    #[tokio::test]
    async fn test_full_close_discards_queued_backlog() {
        // Distinguishes FULL close from half-close: a client that dies with
        // frames still queued behind an in-flight hang gets its in-flight
        // handler dropped AND its backlog discarded — none of the queued
        // requests execute (a dead client's work is never run).
        use std::sync::atomic::AtomicBool;
        use std::sync::atomic::AtomicUsize;

        let counted = Arc::new(AtomicUsize::new(0));
        let counted_h = counted.clone();
        let entered = Arc::new(tokio::sync::Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let dropped_notify = Arc::new(tokio::sync::Notify::new());
        let entered_h = entered.clone();
        let dropped_h = dropped.clone();
        let dropped_notify_h = dropped_notify.clone();
        let router = register_builtin_methods(Router::builder())
            .register("test.hang", move |_: Option<serde_json::Value>| {
                let entered = entered_h.clone();
                let guard = HangGuard {
                    dropped: dropped_h.clone(),
                    notify: dropped_notify_h.clone(),
                };
                async move {
                    entered.notify_one();
                    let _guard = guard;
                    std::future::pending::<()>().await;
                    unreachable!("test.hang never completes")
                }
            })
            .register("test.count", move |_: Option<serde_json::Value>| {
                let counted = counted_h.clone();
                async move {
                    counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(serde_json::json!({"counted": true}))
                }
            })
            .build();

        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("fullclose.sock");
        let cancel = tokio_util::sync::CancellationToken::new();
        let server = Server::new(
            ServerConfig {
                socket_path: socket_path.clone(),
                tcp_addr: String::new(),
                auth_token: None,
            },
            router,
            cancel.clone(),
        );
        let server_handle = tokio::spawn(async move {
            server.run().await.unwrap();
        });

        let stream = connect_with_retries(&socket_path).await;
        let mut framed = Framed::new(stream, create_codec());
        framed
            .send(frame_bytes(&serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": "test.hang"
            })))
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .expect("hang handler never entered");
        // Queue a backlog behind the hang, then FULLY close (both halves).
        for id in 2..8 {
            framed
                .send(frame_bytes(&serde_json::json!({
                    "jsonrpc": "2.0", "id": id, "method": "test.count"
                })))
                .await
                .unwrap();
        }
        drop(framed);

        // In-flight handler dropped promptly (the moment this fires, the
        // executor has taken the close branch — no further dequeues happen).
        tokio::time::timeout(std::time::Duration::from_secs(5), dropped_notify.notified())
            .await
            .expect("in-flight handler not dropped on full close");
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));

        // The dead client's backlog was never executed.
        assert_eq!(
            counted.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a dead client's queued requests must not run"
        );

        cancel.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_handle).await;
    }

    #[tokio::test]
    async fn test_pending_bytes_cap_exceeded_cancels_connection() {
        // greptile PR #90 P1: the frame-count cap alone allows ~4 GiB
        // (256 × 16 MiB) of retained bytes. MAX_PENDING_BYTES severs the
        // connection on QUEUED BYTES — here ~65 MiB across only ~65 frames,
        // far below the 256-frame cap.
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("bytecap.sock");
        let cancel = tokio_util::sync::CancellationToken::new();
        let (router, entered, dropped, dropped_notify) = hang_router();
        let server = Server::new(
            ServerConfig {
                socket_path: socket_path.clone(),
                tcp_addr: String::new(),
                auth_token: None,
            },
            router,
            cancel.clone(),
        );
        let server_handle = tokio::spawn(async move {
            server.run().await.unwrap();
        });

        let stream = connect_with_retries(&socket_path).await;
        let mut framed = Framed::new(stream, create_codec());
        framed
            .send(frame_bytes(&serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": "test.hang"
            })))
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .expect("hang handler never entered");

        // ~1 MiB frames; the byte cap (64 MiB) trips at ~frame 65 — well
        // before the 256-frame cap could. Mid-flood send errors are the
        // severing under test.
        let big = vec![b'x'; 1024 * 1024];
        let frame_count = (MAX_PENDING_BYTES / big.len()) + 4;
        assert!(
            frame_count < MAX_PENDING_FRAMES,
            "test must trip the BYTE cap first ({frame_count} frames)"
        );
        for _ in 0..frame_count {
            if framed.send(Bytes::from(big.clone())).await.is_err() {
                break; // Server already severed the connection.
            }
        }

        // Deliberate cancellation: in-flight handler dropped...
        tokio::time::timeout(std::time::Duration::from_secs(5), dropped_notify.notified())
            .await
            .expect("byte-cap violation did not drop the in-flight handler");
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));

        // ...and the still-open client observes server-side EOF/error.
        let next = tokio::time::timeout(std::time::Duration::from_secs(5), framed.next())
            .await
            .expect("server did not close the violating connection");
        assert!(
            !matches!(next, Some(Ok(_))),
            "expected EOF/error after the byte-cap violation, got a frame: {next:?}"
        );

        cancel.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_handle).await;
    }

    #[tokio::test]
    async fn test_frame_error_response_is_deterministic() {
        // codex P3 round-3: on a decode error the round-2 read task enqueued
        // the Err and IMMEDIATELY cancelled conn_closed, so the executor's
        // outer select could take the already-ready close branch before
        // dequeuing the Err — the UDS frame_too_large response became a coin
        // flip. Now decode errors do NOT cancel conn_closed (only EOF/IO
        // death and the cap do), so while the client is still connected the
        // executor's ONLY ready branch is the queued Err: the error response
        // is deterministic BY STRUCTURE, not by polling order. The 25
        // iterations are a regression belt — the racy round-2 code fails
        // this with p ≈ 1 − 2⁻²⁵. No sleeps: each iteration is pure
        // request/response.
        use tokio::io::AsyncWriteExt;

        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("frame-err.sock");
        let cancel = tokio_util::sync::CancellationToken::new();
        let router = register_builtin_methods(Router::builder()).build();
        let server = Server::new(
            ServerConfig {
                socket_path: socket_path.clone(),
                tcp_addr: String::new(),
                auth_token: None,
            },
            router,
            cancel.clone(),
        );
        let server_handle = tokio::spawn(async move {
            server.run().await.unwrap();
        });

        for iteration in 0..25 {
            let mut stream = connect_with_retries(&socket_path).await;
            // Raw oversized frame: a 4-byte BE length prefix past the codec
            // cap plus a little payload — the decoder rejects it from the
            // length field alone (InvalidData).
            let oversized_len = (MAX_FRAME_SIZE + 1) as u32;
            stream
                .write_all(&oversized_len.to_be_bytes())
                .await
                .unwrap();
            stream.write_all(&[0u8; 64]).await.unwrap();
            stream.flush().await.unwrap();

            // The client MUST receive the frame_too_large error response —
            // every iteration, never bare EOF.
            let mut framed = Framed::new(stream, create_codec());
            let response_frame =
                tokio::time::timeout(std::time::Duration::from_secs(5), framed.next())
                    .await
                    .unwrap_or_else(|_| {
                        panic!("iteration {iteration}: no frame-error response within 5s")
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "iteration {iteration}: bare EOF instead of the \
                             frame_too_large error response (the round-2 race)"
                        )
                    })
                    .expect("response frame decode");
            let response: serde_json::Value = serde_json::from_slice(&response_frame).unwrap();
            assert_eq!(
                response["error"]["code"], -32001,
                "iteration {iteration}: expected frame_too_large, got {response}"
            );

            // And then the server closes the connection.
            let next = tokio::time::timeout(std::time::Duration::from_secs(5), framed.next())
                .await
                .unwrap_or_else(|_| {
                    panic!("iteration {iteration}: connection not closed after frame error")
                });
            assert!(
                !matches!(next, Some(Ok(_))),
                "iteration {iteration}: unexpected extra frame: {next:?}"
            );
        }

        cancel.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_handle).await;
    }

    #[tokio::test]
    async fn test_tcp_auth_required() {
        // KNOWN FLAKE: bind→drop→re-bind TOCTOU race on the auto-
        // assigned port. Between `drop(tcp_listener)` and the Server's
        // own `TcpListener::bind`, another process can grab the port
        // (CI runners with parallel jobs are most likely; locally
        // this almost never trips). Tracked in
        // .config/nextest.toml's flake list; a proper fix would
        // hand the listener directly to Server::new instead of
        // round-tripping through a port string. For now the test
        // re-runs on retry per the nextest override.
        //
        // TCP connections with a configured token must authenticate first.
        let cancel = tokio_util::sync::CancellationToken::new();
        let router = register_builtin_methods(Router::builder()).build();

        // Use port 0 to let the OS assign a random available port.
        let tcp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tcp_addr = tcp_listener.local_addr().unwrap();

        let config = ServerConfig {
            // We still need a UDS path even though we're testing TCP.
            socket_path: tempfile::tempdir().unwrap().path().join("test.sock"),
            tcp_addr: tcp_addr.to_string(),
            auth_token: Some("test-token-42".to_string()),
        };

        // Drop the pre-bound listener so the server can bind.
        drop(tcp_listener);

        let server = Server::new(config, router, cancel.clone());

        let server_handle = tokio::spawn(async move {
            server.run().await.unwrap();
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Connect via TCP without authenticating first.
        let stream = tokio::net::TcpStream::connect(tcp_addr).await.unwrap();
        let mut framed = Framed::new(stream, create_codec());

        // Send a non-auth request -- should be rejected.
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "system.version"
        });
        let request_bytes = serde_json::to_vec(&request).unwrap();
        framed.send(Bytes::from(request_bytes)).await.unwrap();

        let response_frame = framed.next().await.unwrap().unwrap();
        let response: serde_json::Value = serde_json::from_slice(&response_frame).unwrap();

        // Should get an AuthRequired error.
        assert_eq!(response["error"]["code"], -32003);
        assert_eq!(response["error"]["message"], "auth_required");

        // Connection should be closed after auth failure -- next read returns None.
        let next = framed.next().await;
        assert!(
            next.is_none(),
            "connection should be closed after auth failure"
        );

        // Now test successful auth flow.
        let stream2 = tokio::net::TcpStream::connect(tcp_addr).await.unwrap();
        let mut framed2 = Framed::new(stream2, create_codec());

        // Send auth request with correct token.
        let auth_request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "auth",
            "params": {"token": "test-token-42"}
        });
        let auth_bytes = serde_json::to_vec(&auth_request).unwrap();
        framed2.send(Bytes::from(auth_bytes)).await.unwrap();

        let auth_response_frame = framed2.next().await.unwrap().unwrap();
        let auth_response: serde_json::Value =
            serde_json::from_slice(&auth_response_frame).unwrap();
        assert_eq!(auth_response["result"]["authenticated"], true);

        // Now send a normal request -- should work.
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "system.health"
        });
        let request_bytes = serde_json::to_vec(&request).unwrap();
        framed2.send(Bytes::from(request_bytes)).await.unwrap();

        let response_frame = framed2.next().await.unwrap().unwrap();
        let response: serde_json::Value = serde_json::from_slice(&response_frame).unwrap();
        assert_eq!(response["result"]["status"], "ok");

        // Shutdown.
        cancel.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_handle).await;
    }
}
