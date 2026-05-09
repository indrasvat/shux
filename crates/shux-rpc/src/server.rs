//! JSON-RPC server — UDS and optional TCP listeners.
//!
//! Accepts connections, frames messages with LengthDelimitedCodec,
//! parses JSON-RPC 2.0 requests, dispatches to the router, and
//! sends responses back over the same connection.

use std::path::PathBuf;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use tokio::net::{TcpListener, UnixListener, UnixStream};
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

/// Handle a UDS client connection.
///
/// UDS connections are trusted (no auth needed — PRD §13.1).
async fn handle_uds_connection(
    stream: UnixStream,
    router: Router,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut framed = Framed::new(stream, create_codec());

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            frame = framed.next() => {
                match frame {
                    Some(Ok(data)) => {
                        let response = process_frame(&data, &router).await;
                        let response_bytes = serde_json::to_vec(&response)?;
                        framed.send(Bytes::from(response_bytes)).await?;
                    }
                    Some(Err(e)) => {
                        // Frame too large or codec error.
                        warn!(error = %e, "frame error, closing connection");
                        // Send error response if possible.
                        let error_response = JsonRpcResponse::error(
                            None,
                            RpcError::frame_too_large(0, MAX_FRAME_SIZE),
                        );
                        if let Ok(bytes) = serde_json::to_vec(&error_response) {
                            let _ = framed.send(Bytes::from(bytes)).await;
                        }
                        break;
                    }
                    None => {
                        // Client disconnected.
                        debug!("client disconnected");
                        break;
                    }
                }
            }
        }
    }

    Ok(())
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
    let mut framed = Framed::new(stream, create_codec());
    let mut authenticated = expected_token.is_none(); // No token configured = no auth needed.

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            frame = framed.next() => {
                match frame {
                    Some(Ok(data)) => {
                        // If not yet authenticated, the first message must be an auth request.
                        if !authenticated {
                            match try_authenticate(&data, &expected_token) {
                                Ok(true) => {
                                    authenticated = true;
                                    let response = JsonRpcResponse::success(
                                        Some(serde_json::json!(0)),
                                        serde_json::json!({"authenticated": true}),
                                    );
                                    let bytes = serde_json::to_vec(&response)?;
                                    framed.send(Bytes::from(bytes)).await?;
                                    continue;
                                }
                                Ok(false) | Err(_) => {
                                    let error_response = JsonRpcResponse::error(
                                        None,
                                        RpcError::new(ErrorCode::AuthRequired),
                                    );
                                    let bytes = serde_json::to_vec(&error_response)?;
                                    framed.send(Bytes::from(bytes)).await?;
                                    break; // Close connection on auth failure.
                                }
                            }
                        }

                        let response = process_frame(&data, &router).await;
                        let response_bytes = serde_json::to_vec(&response)?;
                        framed.send(Bytes::from(response_bytes)).await?;
                    }
                    Some(Err(e)) => {
                        warn!(error = %e, "TCP frame error, closing connection");
                        break;
                    }
                    None => {
                        debug!("TCP client disconnected");
                        break;
                    }
                }
            }
        }
    }

    Ok(())
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
    builder
        .register(
            "system.version",
            |_params: Option<serde_json::Value>| async {
                Ok(serde_json::json!({
                    "version": env!("CARGO_PKG_VERSION"),
                    "git_sha": env!("SHUX_GIT_SHA"),
                    "name": "shux",
                }))
            },
        )
        .register(
            "system.health",
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

    #[tokio::test]
    async fn test_tcp_auth_required() {
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
