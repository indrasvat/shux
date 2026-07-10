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
    let (read_half, write_half) = stream.into_split();
    let auth = match expected_token {
        Some(token) => ConnAuth::Pending(token),
        None => ConnAuth::Trusted, // No token configured = no auth needed.
    };
    // Historical TCP behavior: close silently on codec errors.
    serve_connection(read_half, write_half, router, cancel, auth, false).await
}

/// Serve one framed JSON-RPC connection with CANCELLABLE request execution.
///
/// Architecture (P3 codex B2 / claude major — client disconnect must drop
/// in-flight request futures): the connection is split so the READ side keeps
/// running while a request executes.
///
/// - A spawned read task decodes frames and queues them on a bounded channel;
///   on EOF or read error it cancels `conn_closed`. Because it reads
///   independently of request execution, a client disconnect is observed
///   IMMEDIATELY — not after the in-flight handler returns (previously an
///   abandoned `pane.wait_settled` lived until settle or its 600s cap).
/// - This executor drains the queue STRICTLY SERIALLY (response order equals
///   request order — exactly the pre-split semantics) and races each dispatch
///   against `conn_closed`: when the client goes away, the in-flight handler
///   FUTURE IS DROPPED, releasing whatever it held (settle watch
///   subscriptions, event long-polls, locks-on-await).
///
/// Bounded-queue note: a client pipelining more than the queue capacity can
/// delay EOF detection until the executor drains below capacity; the common
/// case (one request in flight) detects disconnect instantly.
async fn serve_connection<R, W>(
    read_half: R,
    write_half: W,
    router: Router,
    cancel: tokio_util::sync::CancellationToken,
    mut auth: ConnAuth,
    error_response_on_bad_frame: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio_util::codec::{FramedRead, FramedWrite};

    let mut framed_tx = FramedWrite::new(write_half, create_codec());
    let (frame_tx, mut frame_rx) =
        tokio::sync::mpsc::channel::<Result<bytes::BytesMut, std::io::Error>>(8);
    let conn_closed = tokio_util::sync::CancellationToken::new();

    let read_closed = conn_closed.clone();
    let read_task = tokio::spawn(async move {
        let mut framed_rx = FramedRead::new(read_half, create_codec());
        loop {
            match framed_rx.next().await {
                Some(Ok(frame)) => {
                    if frame_tx.send(Ok(frame)).await.is_err() {
                        break; // Executor gone; stop reading.
                    }
                }
                Some(Err(e)) => {
                    let _ = frame_tx.send(Err(e)).await;
                    break;
                }
                None => break, // Client closed its write side (EOF).
            }
        }
        // Signal close AFTER queueing everything read so far. The dropped
        // frame_tx also lets the executor drain to None and finish.
        read_closed.cancel();
    });

    // Executor loop. Errors (`?` on writes) must still abort the read task,
    // so run the loop in an inner async block and join on `result` below.
    let result: Result<(), Box<dyn std::error::Error + Send + Sync>> = async {
        loop {
            let maybe_frame = tokio::select! {
                _ = cancel.cancelled() => break,
                m = frame_rx.recv() => m,
            };
            let data = match maybe_frame {
                None => {
                    // Read side finished and the queue is drained.
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
                Some(Ok(data)) => data,
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

    // Stop the read task on every exit path (server shutdown, write error,
    // auth failure) — otherwise it would idle on a still-open socket forever.
    read_task.abort();
    let _ = read_task.await;
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
