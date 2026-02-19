# 008 — JSON-RPC Server Foundation

**Status:** Pending
**Depends On:** 001, 002
**Parallelizable With:** 005, 006

---

## Problem

shux's architecture treats the CLI as a thin wrapper over JSON-RPC calls (PRD §4.3 invariant 2: CLI == API). Every operation — creating sessions, splitting panes, capturing output — flows through the JSON-RPC server. This task builds the server foundation: the transport layer (Unix domain socket with optional TCP), the framing codec, the JSON-RPC 2.0 parser, a method router, and three initial methods for health checking and session listing.

The server must handle concurrent clients, enforce a 16 MB max frame size (PRD §8.1), set secure socket permissions, and provide a clean error system with standard JSON-RPC error codes plus shux-specific codes.

This is a foundation task — later tasks (035+) will add the full API surface. This task focuses on getting the transport, framing, routing, and error handling right.

## PRD Reference

- §8 — API design (JSON-RPC primary, gRPC optional)
- §8.1 — Transport: UDS (always on), TCP 127.0.0.1 (opt-in with auth token), max frame size 16 MB
- §8.3 — Request/response format (JSON-RPC 2.0)
- §13.1 — Security: UDS with 0700 permissions, token-based TCP auth
- §15.2 — Technology choices: tokio::net::UnixListener + tokio_util::codec::LengthDelimitedCodec + json-rpc-types + serde_json
- §4.3 — CLI == API invariant

---

## Files to Create

- `crates/shux-rpc/src/error.rs` — JSON-RPC error codes and error construction
- `crates/shux-rpc/src/codec.rs` — LengthDelimitedCodec configuration and frame validation
- `crates/shux-rpc/src/router.rs` — Method router (dispatch table)
- `crates/shux-rpc/src/server.rs` — UDS + TCP listener, client connection handling
- `crates/shux-rpc/src/lib.rs` — Public API, module declarations (replaces stub)

## Files to Modify

- `crates/shux-rpc/Cargo.toml` — Add dependencies

---

## Execution Steps

### Step 1: Add dependencies to shux-rpc

Update `crates/shux-rpc/Cargo.toml`:

```toml
[package]
name = "shux-rpc"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
tokio = { workspace = true }
tokio-util = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
json-rpc-types = { workspace = true }
tracing = { workspace = true }
thiserror = { workspace = true }
uuid = { workspace = true }
futures = "0.3"
bytes = "1"

# For socket permissions on Unix
[target.'cfg(unix)'.dependencies]
nix = { workspace = true }

[dev-dependencies]
tokio = { workspace = true, features = ["test-util", "macros"] }
tempfile = { workspace = true }
```

### Step 2: Define JSON-RPC error codes (`error.rs`)

The error system defines standard JSON-RPC 2.0 error codes plus shux-specific codes (PRD §8.3).

```rust
//! JSON-RPC error codes and error construction.
//!
//! Standard JSON-RPC 2.0 codes (-32600 to -32603) plus shux-specific
//! codes (-32001 to -32099).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

/// JSON-RPC error codes.
///
/// Standard codes (-32600..-32699) as defined by the JSON-RPC 2.0 spec.
/// Custom codes (-32001..-32099) for shux-specific errors (PRD §8.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    // ── Standard JSON-RPC 2.0 codes ───────────────────────────

    /// Invalid JSON was received by the server.
    ParseError,
    /// The JSON sent is not a valid Request object.
    InvalidRequest,
    /// The method does not exist or is not available.
    MethodNotFound,
    /// Invalid method parameter(s).
    InvalidParams,
    /// Internal JSON-RPC error.
    InternalError,

    // ── shux-specific codes ───────────────────────────────────

    /// The frame exceeds the 16 MB maximum size.
    FrameTooLarge,
    /// Optimistic concurrency conflict (version mismatch).
    VersionConflict,
    /// Authentication required (TCP connections without valid token).
    AuthRequired,
    /// Resource not found (session, window, pane does not exist).
    NotFound,
    /// Operation not permitted (insufficient plugin permissions, etc.).
    PermissionDenied,
    /// Rate limit exceeded.
    RateLimited,
}

impl ErrorCode {
    /// The integer code for this error.
    pub fn code(self) -> i64 {
        match self {
            ErrorCode::ParseError => -32700,
            ErrorCode::InvalidRequest => -32600,
            ErrorCode::MethodNotFound => -32601,
            ErrorCode::InvalidParams => -32602,
            ErrorCode::InternalError => -32603,
            ErrorCode::FrameTooLarge => -32001,
            ErrorCode::VersionConflict => -32002,
            ErrorCode::AuthRequired => -32003,
            ErrorCode::NotFound => -32004,
            ErrorCode::PermissionDenied => -32005,
            ErrorCode::RateLimited => -32006,
        }
    }

    /// The default message for this error code.
    pub fn message(self) -> &'static str {
        match self {
            ErrorCode::ParseError => "parse_error",
            ErrorCode::InvalidRequest => "invalid_request",
            ErrorCode::MethodNotFound => "method_not_found",
            ErrorCode::InvalidParams => "invalid_params",
            ErrorCode::InternalError => "internal_error",
            ErrorCode::FrameTooLarge => "frame_too_large",
            ErrorCode::VersionConflict => "version_conflict",
            ErrorCode::AuthRequired => "auth_required",
            ErrorCode::NotFound => "not_found",
            ErrorCode::PermissionDenied => "permission_denied",
            ErrorCode::RateLimited => "rate_limited",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.message(), self.code())
    }
}

/// A JSON-RPC error response object.
///
/// Follows the JSON-RPC 2.0 error format (PRD §8.3):
/// ```json
/// {
///   "code": -32601,
///   "message": "method_not_found",
///   "data": { "method": "session.frobnicate" }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    /// Create a new RPC error from an error code.
    pub fn new(code: ErrorCode) -> Self {
        RpcError {
            code: code.code(),
            message: code.message().to_string(),
            data: None,
        }
    }

    /// Create a new RPC error with additional data.
    pub fn with_data(code: ErrorCode, data: Value) -> Self {
        RpcError {
            code: code.code(),
            message: code.message().to_string(),
            data: Some(data),
        }
    }

    /// Create a new RPC error with a custom message.
    pub fn with_message(code: ErrorCode, message: impl Into<String>) -> Self {
        RpcError {
            code: code.code(),
            message: message.into(),
            data: None,
        }
    }

    /// Create a new RPC error with a custom message and data.
    pub fn with_message_and_data(
        code: ErrorCode,
        message: impl Into<String>,
        data: Value,
    ) -> Self {
        RpcError {
            code: code.code(),
            message: message.into(),
            data: Some(data),
        }
    }

    // ── Convenience constructors ──────────────────────────────

    /// Method not found error.
    pub fn method_not_found(method: &str) -> Self {
        Self::with_data(
            ErrorCode::MethodNotFound,
            serde_json::json!({ "method": method }),
        )
    }

    /// Invalid params error.
    pub fn invalid_params(detail: &str) -> Self {
        Self::with_data(
            ErrorCode::InvalidParams,
            serde_json::json!({ "detail": detail }),
        )
    }

    /// Internal error.
    pub fn internal(detail: &str) -> Self {
        Self::with_data(
            ErrorCode::InternalError,
            serde_json::json!({ "detail": detail }),
        )
    }

    /// Frame too large error.
    pub fn frame_too_large(size: usize, max: usize) -> Self {
        Self::with_data(
            ErrorCode::FrameTooLarge,
            serde_json::json!({
                "size": size,
                "max_size": max,
                "hint": "Reduce the request payload size"
            }),
        )
    }

    /// Version conflict error (optimistic concurrency).
    pub fn version_conflict(
        resource: &str,
        id: &str,
        expected: u64,
        actual: u64,
    ) -> Self {
        Self::with_data(
            ErrorCode::VersionConflict,
            serde_json::json!({
                "resource": resource,
                "id": id,
                "expected_version": expected,
                "actual_version": actual,
                "hint": "Re-read the resource state and retry with the current version"
            }),
        )
    }

    /// Not found error.
    pub fn not_found(resource: &str, id: &str) -> Self {
        Self::with_data(
            ErrorCode::NotFound,
            serde_json::json!({
                "resource": resource,
                "id": id,
            }),
        )
    }
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RPC error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for RpcError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_code_values() {
        assert_eq!(ErrorCode::ParseError.code(), -32700);
        assert_eq!(ErrorCode::InvalidRequest.code(), -32600);
        assert_eq!(ErrorCode::MethodNotFound.code(), -32601);
        assert_eq!(ErrorCode::InvalidParams.code(), -32602);
        assert_eq!(ErrorCode::InternalError.code(), -32603);
        assert_eq!(ErrorCode::FrameTooLarge.code(), -32001);
        assert_eq!(ErrorCode::VersionConflict.code(), -32002);
    }

    #[test]
    fn test_error_serialization() {
        let err = RpcError::method_not_found("session.frobnicate");
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["code"], -32601);
        assert_eq!(json["message"], "method_not_found");
        assert_eq!(json["data"]["method"], "session.frobnicate");
    }

    #[test]
    fn test_version_conflict_error() {
        let err = RpcError::version_conflict("pane", "abc-123", 3, 5);
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["code"], -32002);
        assert_eq!(json["data"]["expected_version"], 3);
        assert_eq!(json["data"]["actual_version"], 5);
        assert!(json["data"]["hint"].as_str().unwrap().contains("Re-read"));
    }

    #[test]
    fn test_frame_too_large_error() {
        let err = RpcError::frame_too_large(20_000_000, 16_777_216);
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["code"], -32001);
        assert_eq!(json["data"]["size"], 20_000_000);
    }
}
```

### Step 3: Configure the framing codec (`codec.rs`)

The codec uses `tokio_util::codec::LengthDelimitedCodec` with 4-byte big-endian length prefixes and a 16 MB maximum frame size.

```rust
//! Framing codec for the JSON-RPC transport.
//!
//! Uses length-prefixed framing: 4-byte big-endian length + JSON payload.
//! Same codec is shared between the UDS/TCP JSON-RPC server and the
//! process plugin protocol (PRD §8.1).

use tokio_util::codec::LengthDelimitedCodec;

/// Maximum frame size: 16 MB (PRD §8.1).
///
/// Frames exceeding this limit are rejected immediately with error code
/// -32001 (frame_too_large) and the connection is closed.
pub const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024; // 16 MB

/// Length prefix size: 4 bytes big-endian.
pub const LENGTH_PREFIX_SIZE: usize = 4;

/// Create the framing codec for JSON-RPC transport.
///
/// Configuration:
/// - 4-byte big-endian length prefix
/// - Maximum frame length: 16 MB
/// - Length field does NOT include itself (just the payload length)
pub fn create_codec() -> LengthDelimitedCodec {
    LengthDelimitedCodec::builder()
        .big_endian()
        .length_field_length(LENGTH_PREFIX_SIZE)
        .max_frame_length(MAX_FRAME_SIZE)
        .new_codec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::{BufMut, BytesMut};
    use tokio_util::codec::{Decoder, Encoder};

    #[test]
    fn test_codec_roundtrip() {
        let mut codec = create_codec();
        let payload = b"hello world";

        // Encode.
        let mut buf = BytesMut::new();
        codec
            .encode(bytes::Bytes::from_static(payload), &mut buf)
            .unwrap();

        // Should have 4-byte length prefix + payload.
        assert_eq!(buf.len(), LENGTH_PREFIX_SIZE + payload.len());

        // Decode.
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(&decoded[..], payload);
    }

    #[test]
    fn test_codec_max_frame_size() {
        let mut codec = create_codec();
        let mut buf = BytesMut::new();

        // Write a length prefix indicating a frame larger than MAX_FRAME_SIZE.
        let oversized_len = (MAX_FRAME_SIZE + 1) as u32;
        buf.put_u32(oversized_len);
        // Add some dummy data.
        buf.extend_from_slice(&[0u8; 100]);

        // Decoding should fail.
        let result = codec.decode(&mut buf);
        assert!(result.is_err(), "expected error for oversized frame");
    }

    #[test]
    fn test_codec_empty_frame() {
        let mut codec = create_codec();
        let payload = b"";

        let mut buf = BytesMut::new();
        codec
            .encode(bytes::Bytes::from_static(payload), &mut buf)
            .unwrap();

        // 4-byte prefix + 0 bytes payload.
        assert_eq!(buf.len(), LENGTH_PREFIX_SIZE);

        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn test_codec_json_payload() {
        let mut codec = create_codec();
        let json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "system.version"
        });
        let payload = serde_json::to_vec(&json).unwrap();

        let mut buf = BytesMut::new();
        codec
            .encode(bytes::Bytes::from(payload.clone()), &mut buf)
            .unwrap();

        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(parsed["method"], "system.version");
    }
}
```

### Step 4: Implement the method router (`router.rs`)

The router maps JSON-RPC method names to handler functions. Handlers are async functions that take parsed params and return a result or error.

```rust
//! Method router — dispatches JSON-RPC requests to handler functions.
//!
//! The router maintains a HashMap of method names to handler trait objects.
//! New methods are registered via `router.register("method.name", handler)`.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;
use tracing::{debug, warn};

use crate::error::RpcError;

/// The result type for RPC method handlers.
pub type HandlerResult = Result<Value, RpcError>;

/// A boxed future that resolves to a HandlerResult.
pub type HandlerFuture = Pin<Box<dyn Future<Output = HandlerResult> + Send>>;

/// Trait for RPC method handlers.
///
/// Handlers receive the `params` field from the JSON-RPC request (may be null)
/// and return either a successful result value or an RPC error.
pub trait Handler: Send + Sync + 'static {
    /// Handle a JSON-RPC request.
    fn handle(&self, params: Option<Value>) -> HandlerFuture;
}

/// Implement Handler for async functions.
///
/// This allows registering closures and functions directly:
/// ```ignore
/// router.register("system.version", |_params| async {
///     Ok(serde_json::json!({"version": "0.1.0"}))
/// });
/// ```
impl<F, Fut> Handler for F
where
    F: Fn(Option<Value>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = HandlerResult> + Send + 'static,
{
    fn handle(&self, params: Option<Value>) -> HandlerFuture {
        Box::pin((self)(params))
    }
}

/// The method router.
///
/// Thread-safe (uses Arc internally for handler storage).
/// Cloning is cheap.
#[derive(Clone)]
pub struct Router {
    methods: Arc<HashMap<String, Arc<dyn Handler>>>,
}

impl Router {
    /// Create a new empty router.
    pub fn new() -> Self {
        Router {
            methods: Arc::new(HashMap::new()),
        }
    }

    /// Create a router builder for registering methods.
    pub fn builder() -> RouterBuilder {
        RouterBuilder {
            methods: HashMap::new(),
        }
    }

    /// Dispatch a JSON-RPC request to the appropriate handler.
    ///
    /// Returns `Err(RpcError)` if the method is not found.
    pub async fn dispatch(&self, method: &str, params: Option<Value>) -> HandlerResult {
        match self.methods.get(method) {
            Some(handler) => {
                debug!(method, "dispatching RPC method");
                handler.handle(params).await
            }
            None => {
                warn!(method, "method not found");
                Err(RpcError::method_not_found(method))
            }
        }
    }

    /// List all registered method names.
    pub fn methods(&self) -> Vec<&str> {
        self.methods.keys().map(|s| s.as_str()).collect()
    }

    /// Check if a method is registered.
    pub fn has_method(&self, method: &str) -> bool {
        self.methods.contains_key(method)
    }
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Router {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Router")
            .field("method_count", &self.methods.len())
            .field("methods", &self.methods())
            .finish()
    }
}

/// Builder for constructing a Router.
///
/// Methods are registered on the builder, then `.build()` produces
/// an immutable Router.
pub struct RouterBuilder {
    methods: HashMap<String, Arc<dyn Handler>>,
}

impl RouterBuilder {
    /// Register a method handler.
    pub fn register(mut self, method: impl Into<String>, handler: impl Handler) -> Self {
        self.methods.insert(method.into(), Arc::new(handler));
        self
    }

    /// Build the router.
    pub fn build(self) -> Router {
        Router {
            methods: Arc::new(self.methods),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_dispatch_registered_method() {
        let router = Router::builder()
            .register("system.version", |_params: Option<Value>| async {
                Ok(serde_json::json!({"version": "0.1.0"}))
            })
            .build();

        let result = router.dispatch("system.version", None).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap()["version"], "0.1.0");
    }

    #[tokio::test]
    async fn test_dispatch_unknown_method() {
        let router = Router::builder().build();

        let result = router.dispatch("nonexistent.method", None).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, -32601);
    }

    #[tokio::test]
    async fn test_dispatch_with_params() {
        let router = Router::builder()
            .register("echo", |params: Option<Value>| async move {
                Ok(params.unwrap_or(Value::Null))
            })
            .build();

        let params = serde_json::json!({"name": "shux"});
        let result = router.dispatch("echo", Some(params.clone())).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), params);
    }

    #[test]
    fn test_router_methods_list() {
        let router = Router::builder()
            .register("system.version", |_: Option<Value>| async {
                Ok(Value::Null)
            })
            .register("system.health", |_: Option<Value>| async {
                Ok(Value::Null)
            })
            .build();

        let methods = router.methods();
        assert_eq!(methods.len(), 2);
        assert!(router.has_method("system.version"));
        assert!(router.has_method("system.health"));
        assert!(!router.has_method("nonexistent"));
    }

    #[test]
    fn test_router_is_clone() {
        let router = Router::builder()
            .register("test", |_: Option<Value>| async { Ok(Value::Null) })
            .build();

        let cloned = router.clone();
        assert!(cloned.has_method("test"));
    }
}
```

### Step 5: Implement the server (`server.rs`)

The server manages UDS and optional TCP listeners, accepts connections, frames messages with the codec, parses JSON-RPC requests, dispatches to the router, and sends responses.

```rust
//! JSON-RPC server — UDS and optional TCP listeners.
//!
//! Accepts connections, frames messages with LengthDelimitedCodec,
//! parses JSON-RPC 2.0 requests, dispatches to the router, and
//! sends responses back over the same connection.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use tokio::net::{TcpListener, UnixListener, UnixStream};
use tokio::sync::CancellationToken;
use tokio_util::codec::Framed;
use tracing::{debug, error, info, warn};

use crate::codec::{create_codec, MAX_FRAME_SIZE};
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
    cancel: CancellationToken,
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
    pub fn new(config: ServerConfig, router: Router, cancel: CancellationToken) -> Self {
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
    cancel: CancellationToken,
) -> Result<(), Box<dyn std::error::Error>> {
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
                            RpcError::new(ErrorCode::FrameTooLarge),
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
    cancel: CancellationToken,
    expected_token: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
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
async fn process_frame(
    data: &[u8],
    router: &Router,
) -> JsonRpcResponse {
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
) -> Result<bool, Box<dyn std::error::Error>> {
    let request: JsonRpcRequest = serde_json::from_slice(data)?;
    if request.method != "auth" {
        return Ok(false);
    }
    let Some(ref expected) = expected_token else {
        return Ok(true); // No token required.
    };
    let Some(params) = request.params else {
        return Ok(false);
    };
    let token = params.get("token").and_then(|v| v.as_str());
    Ok(token == Some(expected.as_str()))
}

/// Register the initial built-in methods on a router builder.
///
/// Initial methods (PRD §8.2, M0 scope):
/// - system.version — returns version info
/// - system.health — returns health status
/// - session.list — returns list of sessions (stub for M0)
pub fn register_builtin_methods(builder: crate::router::RouterBuilder) -> crate::router::RouterBuilder {
    builder
        .register("system.version", |_params: Option<serde_json::Value>| async {
            Ok(serde_json::json!({
                "version": env!("CARGO_PKG_VERSION"),
                "name": "shux",
            }))
        })
        .register("system.health", |_params: Option<serde_json::Value>| async {
            Ok(serde_json::json!({
                "status": "ok",
                "uptime_secs": 0, // TODO: track actual uptime (task 001)
            }))
        })
        .register("session.list", |_params: Option<serde_json::Value>| async {
            // Stub — will be connected to SessionGraph in task 013.
            Ok(serde_json::json!({
                "sessions": []
            }))
        })
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
        assert!(router.has_method("session.list"));

        let result = router.dispatch("system.version", None).await.unwrap();
        assert_eq!(result["name"], "shux");

        let result = router.dispatch("system.health", None).await.unwrap();
        assert_eq!(result["status"], "ok");

        let result = router.dispatch("session.list", None).await.unwrap();
        assert!(result["sessions"].as_array().unwrap().is_empty());
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

        let cancel = CancellationToken::new();
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
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            server_handle,
        ).await;
    }
}
```

### Step 6: Wire it all together in `lib.rs`

```rust
//! shux-rpc — JSON-RPC server for shux.
//!
//! Provides the JSON-RPC 2.0 server over Unix domain socket (always on)
//! and optional TCP loopback. Uses length-prefixed framing (4-byte BE +
//! JSON payload). Includes a method router and error system.

pub mod codec;
pub mod error;
pub mod router;
pub mod server;

// Re-export key types.
pub use codec::{create_codec, MAX_FRAME_SIZE};
pub use error::{ErrorCode, RpcError};
pub use router::{Handler, Router, RouterBuilder};
pub use server::{Server, ServerConfig};
```

---

## Verification

### Functional

```bash
# Build the shux-rpc crate
cargo build -p shux-rpc

# Check for clippy warnings
cargo clippy -p shux-rpc -- -D warnings

# Format check
cargo fmt -p shux-rpc -- --check
```

### Tests

```bash
# Run all shux-rpc tests
cargo nextest run -p shux-rpc

# Run with output
cargo nextest run -p shux-rpc --no-capture

# Run specific test modules
cargo nextest run -p shux-rpc -- codec::tests
cargo nextest run -p shux-rpc -- error::tests
cargo nextest run -p shux-rpc -- router::tests
cargo nextest run -p shux-rpc -- server::tests
cargo nextest run -p shux-rpc -- server::tests::tcp_auth_required
```

### Integration test (manual, optional)

```bash
# After the server is running (requires task 001 for daemon):
# Use socat to test UDS directly:
echo '{"jsonrpc":"2.0","id":1,"method":"system.version"}' | \
  socat - UNIX-CONNECT:/tmp/shux/shux.sock

# Expected response:
# {"jsonrpc":"2.0","id":1,"result":{"version":"0.1.0","name":"shux"}}
```

Note: The socat test requires the length-prefix framing. A raw `echo | socat` won't work without prefixing the 4-byte length. The integration test in `server.rs` uses the proper codec.

---

## Completion Criteria

- [ ] `crates/shux-rpc/src/error.rs` — ErrorCode enum with standard JSON-RPC codes + shux-specific codes
- [ ] `crates/shux-rpc/src/error.rs` — RpcError struct with serialization, convenience constructors
- [ ] `crates/shux-rpc/src/error.rs` — Error codes: ParseError (-32700), InvalidRequest (-32600), MethodNotFound (-32601), InvalidParams (-32602), InternalError (-32603), FrameTooLarge (-32001), VersionConflict (-32002)
- [ ] `crates/shux-rpc/src/codec.rs` — LengthDelimitedCodec configured with 4-byte BE prefix and 16 MB max
- [ ] `crates/shux-rpc/src/codec.rs` — Codec roundtrip tests pass
- [ ] `crates/shux-rpc/src/codec.rs` — Oversized frame rejection test passes
- [ ] `crates/shux-rpc/src/router.rs` — Router with HashMap<String, Box<dyn Handler>> dispatch
- [ ] `crates/shux-rpc/src/router.rs` — RouterBuilder for registration
- [ ] `crates/shux-rpc/src/router.rs` — Handler trait implemented for async closures
- [ ] `crates/shux-rpc/src/router.rs` — Unknown method returns MethodNotFound error
- [ ] `crates/shux-rpc/src/server.rs` — UDS listener (tokio::net::UnixListener)
- [ ] `crates/shux-rpc/src/server.rs` — TCP listener (opt-in, with auth token)
- [ ] `crates/shux-rpc/src/server.rs` — Socket file permissions set to 0700
- [ ] `crates/shux-rpc/src/server.rs` — JSON-RPC 2.0 request parsing and validation
- [ ] `crates/shux-rpc/src/server.rs` — JSON-RPC 2.0 response construction (success + error)
- [ ] `crates/shux-rpc/src/server.rs` — Frame error handling (send error, close connection)
- [ ] `crates/shux-rpc/src/server.rs` — TCP auth token validation
- [ ] Explicit auth regression test: TCP requests without token are rejected with `AuthRequired`
- [ ] `crates/shux-rpc/src/server.rs` — Graceful shutdown via CancellationToken
- [ ] `crates/shux-rpc/src/server.rs` — Built-in methods: system.version, system.health, session.list
- [ ] `crates/shux-rpc/src/server.rs` — Stale socket cleanup on startup
- [ ] `crates/shux-rpc/src/server.rs` — Integration test: connect to UDS, send request, receive response
- [ ] `crates/shux-rpc/src/lib.rs` — Module declarations and re-exports
- [ ] `crates/shux-rpc/Cargo.toml` — Dependencies: tokio, tokio-util, serde, serde_json, json-rpc-types, futures, bytes, nix, tracing, thiserror, uuid
- [ ] All unit tests pass
- [ ] Integration test (UDS roundtrip) passes
- [ ] `cargo clippy -p shux-rpc -- -D warnings` passes
- [ ] `cargo fmt -p shux-rpc -- --check` passes

---

## Commit Message

```
feat(rpc): implement JSON-RPC server with UDS, framing, and method router

- Unix domain socket listener with 0700 permissions
- Optional TCP loopback listener with token auth
- LengthDelimitedCodec (4-byte BE prefix, 16 MB max frame)
- JSON-RPC 2.0 request/response parsing and validation
- Method router with async handler dispatch
- Error code system (standard JSON-RPC + shux-specific codes)
- Built-in methods: system.version, system.health, session.list
- Graceful shutdown via CancellationToken
- Integration test for UDS request-response roundtrip
```

---

## Session Protocol

1. **Before starting:** Read `CLAUDE.md`, `docs/PRD.md` §8 (API design), §8.1 (Transport), §8.3 (Request/response format), §13.1 (Security). Verify tasks 001 and 002 are complete or at least that the workspace compiles.
2. **During implementation:**
   - Start with `error.rs` — the error types are used by everything else.
   - Then `codec.rs` — simple but critical. Get the framing right, test oversized frame rejection.
   - Then `router.rs` — the dispatch mechanism. The `Handler` trait must support async closures ergonomically.
   - Finally `server.rs` — this is the most complex file. Start with UDS only, get the integration test passing, then add TCP and auth.
   - Run `cargo clippy -p shux-rpc -- -D warnings` after each file.
3. **Key gotchas:**
   - `tokio_util::codec::LengthDelimitedCodec` uses `Framed<T, LengthDelimitedCodec>` for both `Stream` (reading) and `Sink` (writing). Make sure to import `SinkExt` and `StreamExt` from futures.
   - The `LengthDelimitedCodec::builder().big_endian()` call configures big-endian length prefix. Verify this matches the PRD's "4-byte BE length" specification.
   - Socket cleanup: always try to remove a stale socket file before binding. Otherwise `bind()` fails with "address already in use."
   - Socket permissions: use `std::os::unix::fs::PermissionsExt` to set 0700. This is Unix-only — guard with `#[cfg(unix)]`.
   - The `json-rpc-types` crate provides type definitions but we implement parsing ourselves with serde for more control. The crate is useful for reference but not strictly required in the implementation.
   - The integration test must use `tokio::time::sleep` to give the server time to bind before connecting. Use a short delay (100ms).
4. **After:** Run full test suite (`cargo nextest run -p shux-rpc`). Update `docs/PROGRESS.md` (mark 008 done). Update `CLAUDE.md` Learnings with any discoveries about LengthDelimitedCodec behavior, UDS socket lifecycle, or JSON-RPC parsing edge cases.
