# 051 — gRPC API (Optional Transport)

**Status:** Pending
**Depends On:** 035
**Parallelizable With:** 048, 049

---

## Problem

The JSON-RPC API (task 035) is shux's primary API, but some clients benefit from typed streaming with protobuf codegen. The PRD specifies an optional gRPC transport built with tonic and prost, supporting both UDS and TCP. This provides language-native client generation (Python, Go, Node, Rust) from published `.proto` files without hand-rolling JSON-RPC clients. The gRPC API mirrors every JSON-RPC method 1:1 so there is no feature gap between transports. TCP gRPC uses the same auth token as JSON-RPC TCP. The feature is opt-in via `grpc_enabled = true` in config.

## PRD Reference

- **SS 6.1** API & automation: "gRPC API: Optional, for typed streaming use cases. tonic over UDS/TCP. Published `.proto` files."
- **SS 8.1** Transport table: "gRPC over UDS/TCP: OFF (opt-in). For clients that want typed streaming (protobuf codegen). tonic with UDS connector."
- **SS 15.2** Key crates: "`tonic` + `prost` 0.x — Optional transport. UDS via custom connector (`serve_with_incoming`)."
- **SS 10.2** Config: `grpc_enabled = false` in `[daemon]` section
- **SS 13.1** Security: "gRPC over TCP requires the same auth token as JSON-RPC TCP."

---

## Files to Create

- `proto/shux/v1/shux.proto` — Service definitions matching all JSON-RPC methods
- `proto/shux/v1/types.proto` — Shared protobuf message types
- `proto/shux/v1/events.proto` — Event streaming types
- `crates/shux-rpc/src/grpc.rs` — gRPC server implementation
- `crates/shux-rpc/src/grpc/service.rs` — tonic service implementations
- `crates/shux-rpc/src/grpc/uds.rs` — UDS connector for tonic
- `crates/shux-rpc/src/grpc/auth.rs` — Token auth interceptor for TCP
- `crates/shux-rpc/build.rs` — prost-build proto compilation

## Files to Modify

- `crates/shux-rpc/Cargo.toml` — Add tonic, prost, prost-build dependencies behind `grpc` feature flag
- `crates/shux-rpc/src/lib.rs` — Conditionally expose `grpc` module
- `crates/shux-rpc/src/server.rs` — Start gRPC server alongside JSON-RPC when enabled
- `crates/shux-core/src/config.rs` — Ensure `grpc_enabled` field exists in daemon config
- `docs/PROGRESS.md` — Mark task 051 complete

---

## Execution Steps

### Step 1: Define Protobuf Types

Create `proto/shux/v1/types.proto`:

```protobuf
syntax = "proto3";
package shux.v1;

message SessionInfo {
  string id = 1;
  string name = 2;
  repeated string window_ids = 3;
  string active_window_id = 4;
  uint64 created_at = 5;
  map<string, string> tags = 6;
  uint64 version = 7;
}

message WindowInfo {
  string id = 1;
  string session_id = 2;
  string title = 3;
  repeated string pane_ids = 4;
  string active_pane_id = 5;
  map<string, string> tags = 6;
  uint64 version = 7;
}

message PaneInfo {
  string id = 1;
  string window_id = 2;
  string title = 3;
  string cwd = 4;
  repeated string command = 5;
  optional int32 exit_status = 6;
  string theme = 7;
  map<string, string> tags = 8;
  uint64 version = 9;
}

message SplitDirection {
  enum Direction {
    HORIZONTAL = 0;
    VERTICAL = 1;
  }
  Direction direction = 1;
}

message VersionInfo {
  string version = 1;
  string build_date = 2;
  string git_hash = 3;
}

message HealthInfo {
  string status = 1;  // "ok", "degraded", "unhealthy"
  uint64 uptime_secs = 2;
  uint32 sessions = 3;
  uint32 panes = 4;
}

message PluginInfo {
  string id = 1;
  string name = 2;
  string version = 3;
  string state = 4;
  string kind = 5;
}

message ThemeInfo {
  string name = 1;
  string description = 2;
  string variant = 3;
}

message MetricsSnapshot {
  double render_p50_ms = 1;
  double render_p99_ms = 2;
  double input_p50_ms = 3;
  double input_p99_ms = 4;
  double event_bus_lag_ms = 5;
  uint64 pty_bytes_total = 6;
  uint32 active_sessions = 7;
  uint32 active_panes = 8;
}
```

### Step 2: Define Service Proto

Create `proto/shux/v1/shux.proto`:

```protobuf
syntax = "proto3";
package shux.v1;

import "shux/v1/types.proto";
import "shux/v1/events.proto";

// System service
service System {
  rpc Version(VersionRequest) returns (VersionResponse);
  rpc Health(HealthRequest) returns (HealthResponse);
}

// Session management
service Sessions {
  rpc List(SessionListRequest) returns (SessionListResponse);
  rpc Create(SessionCreateRequest) returns (SessionCreateResponse);
  rpc Ensure(SessionEnsureRequest) returns (SessionEnsureResponse);
  rpc Rename(SessionRenameRequest) returns (SessionRenameResponse);
  rpc Kill(SessionKillRequest) returns (SessionKillResponse);
  rpc Attach(SessionAttachRequest) returns (SessionAttachResponse);
}

// Window management
service Windows {
  rpc List(WindowListRequest) returns (WindowListResponse);
  rpc Create(WindowCreateRequest) returns (WindowCreateResponse);
  rpc Ensure(WindowEnsureRequest) returns (WindowEnsureResponse);
  rpc Rename(WindowRenameRequest) returns (WindowRenameResponse);
  rpc Focus(WindowFocusRequest) returns (WindowFocusResponse);
  rpc Reorder(WindowReorderRequest) returns (WindowReorderResponse);
  rpc Kill(WindowKillRequest) returns (WindowKillResponse);
}

// Pane management
service Panes {
  rpc List(PaneListRequest) returns (PaneListResponse);
  rpc Split(PaneSplitRequest) returns (PaneSplitResponse);
  rpc Ensure(PaneEnsureRequest) returns (PaneEnsureResponse);
  rpc Focus(PaneFocusRequest) returns (PaneFocusResponse);
  rpc Resize(PaneResizeRequest) returns (PaneResizeResponse);
  rpc Zoom(PaneZoomRequest) returns (PaneZoomResponse);
  rpc Swap(PaneSwapRequest) returns (PaneSwapResponse);
  rpc Kill(PaneKillRequest) returns (PaneKillResponse);
  rpc SendKeys(PaneSendKeysRequest) returns (PaneSendKeysResponse);
  rpc RunCommand(PaneRunCommandRequest) returns (PaneRunCommandResponse);
  rpc Capture(PaneCaptureRequest) returns (PaneCaptureResponse);
  rpc SetTitle(PaneSetTitleRequest) returns (PaneSetTitleResponse);
  rpc SetTheme(PaneSetThemeRequest) returns (PaneSetThemeResponse);
  rpc SetTag(PaneSetTagRequest) returns (PaneSetTagResponse);
  rpc GetTags(PaneGetTagsRequest) returns (PaneGetTagsResponse);
}

// State management (batch operations)
service State {
  rpc Snapshot(SnapshotRequest) returns (SnapshotResponse);
  rpc Apply(ApplyRequest) returns (ApplyResponse);
}

// Event streaming
service Events {
  rpc Watch(EventWatchRequest) returns (stream EventNotification);
  rpc History(EventHistoryRequest) returns (EventHistoryResponse);
}

// Theme management
service Themes {
  rpc List(ThemeListRequest) returns (ThemeListResponse);
  rpc Get(ThemeGetRequest) returns (ThemeGetResponse);
  rpc Set(ThemeSetRequest) returns (ThemeSetResponse);
}

// Plugin management
service Plugins {
  rpc List(PluginListRequest) returns (PluginListResponse);
  rpc Enable(PluginEnableRequest) returns (PluginEnableResponse);
  rpc Disable(PluginDisableRequest) returns (PluginDisableResponse);
  rpc Reload(PluginReloadRequest) returns (PluginReloadResponse);
  rpc Inspect(PluginInspectRequest) returns (PluginInspectResponse);
}

// Observability
service Observability {
  rpc DiagnoseRun(DiagnoseRunRequest) returns (DiagnoseRunResponse);
  rpc MetricsGet(MetricsGetRequest) returns (MetricsGetResponse);
  rpc LogTail(LogTailRequest) returns (stream LogEntry);
}

// ─── Request/Response message definitions ───

message VersionRequest {}
message VersionResponse { VersionInfo info = 1; }

message HealthRequest {}
message HealthResponse { HealthInfo info = 1; }

message SessionListRequest {}
message SessionListResponse { repeated SessionInfo sessions = 1; }

message SessionCreateRequest {
  string name = 1;
  string client_request_id = 2;
}
message SessionCreateResponse { SessionInfo session = 1; }

message SessionEnsureRequest { string name = 1; }
message SessionEnsureResponse {
  SessionInfo session = 1;
  bool created = 2;
}

message SessionRenameRequest {
  string session_id = 1;
  string new_name = 2;
  uint64 version = 3;
}
message SessionRenameResponse { SessionInfo session = 1; }

message SessionKillRequest { string session_id = 1; }
message SessionKillResponse {}

message SessionAttachRequest { string session_id = 1; }
message SessionAttachResponse { SessionInfo session = 1; }

message WindowListRequest { string session_id = 1; }
message WindowListResponse { repeated WindowInfo windows = 1; }

message WindowCreateRequest {
  string session_id = 1;
  string name = 2;
  string client_request_id = 3;
}
message WindowCreateResponse { WindowInfo window = 1; }

message WindowEnsureRequest {
  string session_id = 1;
  string name = 2;
}
message WindowEnsureResponse {
  WindowInfo window = 1;
  bool created = 2;
}

message WindowRenameRequest {
  string window_id = 1;
  string new_title = 2;
  uint64 version = 3;
}
message WindowRenameResponse { WindowInfo window = 1; }

message WindowFocusRequest { string window_id = 1; }
message WindowFocusResponse {}

message WindowReorderRequest {
  string window_id = 1;
  uint32 new_index = 2;
}
message WindowReorderResponse {}

message WindowKillRequest { string window_id = 1; }
message WindowKillResponse {}

message PaneListRequest { string window_id = 1; }
message PaneListResponse { repeated PaneInfo panes = 1; }

message PaneSplitRequest {
  string pane_id = 1;
  SplitDirection.Direction direction = 2;
  float ratio = 3;
  repeated string command = 4;
  string client_request_id = 5;
}
message PaneSplitResponse { PaneInfo pane = 1; }

message PaneEnsureRequest {
  string window_id = 1;
  string name = 2;
}
message PaneEnsureResponse {
  PaneInfo pane = 1;
  bool created = 2;
}

message PaneFocusRequest { string pane_id = 1; }
message PaneFocusResponse {}

message PaneResizeRequest {
  string pane_id = 1;
  uint32 width = 2;
  uint32 height = 3;
}
message PaneResizeResponse {}

message PaneZoomRequest { string pane_id = 1; }
message PaneZoomResponse { bool zoomed = 1; }

message PaneSwapRequest {
  string pane_id = 1;
  string target_pane_id = 2;
}
message PaneSwapResponse {}

message PaneKillRequest { string pane_id = 1; }
message PaneKillResponse {}

message PaneSendKeysRequest {
  string pane_id = 1;
  bytes data = 2;
}
message PaneSendKeysResponse {}

message PaneRunCommandRequest {
  string pane_id = 1;
  repeated string command = 2;
  bool async = 3;
  uint32 timeout_secs = 4;
}
message PaneRunCommandResponse {
  int32 exit_code = 1;
  string stdout = 2;
  string stderr = 3;
  string command_id = 4;  // Only set when async=true
}

message PaneCaptureRequest {
  string pane_id = 1;
  uint32 lines = 2;
}
message PaneCaptureResponse { string content = 1; }

message PaneSetTitleRequest {
  string pane_id = 1;
  string title = 2;
}
message PaneSetTitleResponse {}

message PaneSetThemeRequest {
  string pane_id = 1;
  string theme = 2;
}
message PaneSetThemeResponse {}

message PaneSetTagRequest {
  string pane_id = 1;
  string key = 2;
  string value = 3;
}
message PaneSetTagResponse {}

message PaneGetTagsRequest { string pane_id = 1; }
message PaneGetTagsResponse { map<string, string> tags = 1; }

message SnapshotRequest {
  string cursor = 1;
  uint32 page_size = 2;
}
message SnapshotResponse {
  string snapshot_json = 1;
  string next_cursor = 2;
  uint64 sequence = 3;
}

message ApplyRequest {
  string client_request_id = 1;
  string operations_json = 2;  // JSON array of operations
}
message ApplyResponse {
  string results_json = 1;
}

message ThemeListRequest {}
message ThemeListResponse { repeated ThemeInfo themes = 1; }

message ThemeGetRequest { string name = 1; }
message ThemeGetResponse { string theme_toml = 1; }

message ThemeSetRequest {
  string scope = 1;     // "session", "window", "pane"
  string scope_id = 2;
  string theme = 3;
}
message ThemeSetResponse {}

message PluginListRequest {}
message PluginListResponse { repeated PluginInfo plugins = 1; }

message PluginEnableRequest { string plugin_id = 1; }
message PluginEnableResponse {}

message PluginDisableRequest { string plugin_id = 1; }
message PluginDisableResponse {}

message PluginReloadRequest { string plugin_id = 1; }
message PluginReloadResponse {}

message PluginInspectRequest { string plugin_id = 1; }
message PluginInspectResponse { string inspect_json = 1; }

message DiagnoseRunRequest { string redact = 1; }
message DiagnoseRunResponse { string bundle_json = 1; }

message MetricsGetRequest {}
message MetricsGetResponse { MetricsSnapshot metrics = 1; }

message LogTailRequest {
  string plugin_id = 1;  // optional: filter to specific plugin
  string level = 2;      // minimum log level
}
message LogEntry {
  uint64 timestamp = 1;
  string level = 2;
  string message = 3;
  string source = 4;
}
```

### Step 3: Define Event Streaming Proto

Create `proto/shux/v1/events.proto`:

```protobuf
syntax = "proto3";
package shux.v1;

message EventWatchRequest {
  repeated string filters = 1;
  uint64 from_seq = 2;
  uint32 buffer_size = 3;
}

message EventNotification {
  uint64 seq = 1;
  string timestamp = 2;
  string event_type = 3;
  string data_json = 4;
}

message EventGap {
  uint64 from = 1;
  uint64 to = 2;
  uint64 lost = 3;
}

message EventHistoryRequest {
  uint32 count = 1;
  repeated string filters = 2;
}

message EventHistoryResponse {
  repeated EventNotification events = 1;
}
```

### Step 4: Configure prost-build

Create `crates/shux-rpc/build.rs`:

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "grpc")]
    {
        let proto_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("proto");

        tonic_build::configure()
            .build_server(true)
            .build_client(true)  // For testing and internal use
            .out_dir("src/grpc/generated")
            .compile_protos(
                &[
                    proto_root.join("shux/v1/shux.proto"),
                    proto_root.join("shux/v1/types.proto"),
                    proto_root.join("shux/v1/events.proto"),
                ],
                &[proto_root],
            )?;
    }
    Ok(())
}
```

### Step 5: Implement UDS Connector

Create `crates/shux-rpc/src/grpc/uds.rs`:

```rust
//! Unix domain socket connector for tonic gRPC.
//!
//! Allows tonic to serve over UDS instead of TCP, matching
//! the JSON-RPC server's UDS transport.

use std::path::PathBuf;
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::server::Router;

/// Start a gRPC server listening on a Unix domain socket.
pub async fn serve_uds(
    router: Router,
    socket_path: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    // Remove stale socket file if it exists
    let _ = std::fs::remove_file(&socket_path);

    // Create parent directory if needed
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let uds = UnixListener::bind(&socket_path)?;

    // Set socket permissions to 0700 (owner only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o700))?;
    }

    let uds_stream = UnixListenerStream::new(uds);

    tracing::info!(?socket_path, "gRPC server listening on UDS");

    router
        .serve_with_incoming(uds_stream)
        .await?;

    Ok(())
}
```

### Step 6: Implement Token Auth Interceptor

Create `crates/shux-rpc/src/grpc/auth.rs`:

```rust
//! Token-based authentication interceptor for gRPC TCP transport.
//!
//! When gRPC is served over TCP, clients must provide an auth token
//! in the `authorization` metadata header. The token is the same one
//! used for JSON-RPC TCP auth.

use tonic::{Request, Status};

/// Validate the auth token from gRPC metadata.
pub fn check_auth(
    req: Request<()>,
    expected_token: &str,
) -> Result<Request<()>, Status> {
    if expected_token.is_empty() {
        // Auth disabled (UDS transport or explicit opt-out)
        return Ok(req);
    }

    let token = req
        .metadata()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    match token {
        Some(t) if constant_time_eq(t.as_bytes(), expected_token.as_bytes()) => Ok(req),
        Some(_) => Err(Status::unauthenticated("Invalid auth token")),
        None => Err(Status::unauthenticated(
            "Missing authorization header. Use: Bearer <token>",
        )),
    }
}

/// Constant-time comparison to prevent timing attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}
```

### Step 7: Implement gRPC Service

Create `crates/shux-rpc/src/grpc/service.rs` — this delegates all calls to the same handler functions used by the JSON-RPC server:

```rust
//! tonic service implementations.
//!
//! Each gRPC service method delegates to the shared RPC handler layer,
//! ensuring 1:1 parity with JSON-RPC methods.

use crate::handler::RpcHandler;
use tonic::{Request, Response, Status};

// Generated protobuf types
use super::generated::shux::v1::*;

pub struct SystemService {
    handler: RpcHandler,
}

#[tonic::async_trait]
impl system_server::System for SystemService {
    async fn version(
        &self,
        _request: Request<VersionRequest>,
    ) -> Result<Response<VersionResponse>, Status> {
        let info = self.handler.system_version().await.map_err(to_status)?;
        Ok(Response::new(VersionResponse {
            info: Some(info.into()),
        }))
    }

    async fn health(
        &self,
        _request: Request<HealthRequest>,
    ) -> Result<Response<HealthResponse>, Status> {
        let info = self.handler.system_health().await.map_err(to_status)?;
        Ok(Response::new(HealthResponse {
            info: Some(info.into()),
        }))
    }
}

pub struct SessionsService {
    handler: RpcHandler,
}

#[tonic::async_trait]
impl sessions_server::Sessions for SessionsService {
    async fn list(
        &self,
        _request: Request<SessionListRequest>,
    ) -> Result<Response<SessionListResponse>, Status> {
        let sessions = self.handler.session_list().await.map_err(to_status)?;
        Ok(Response::new(SessionListResponse {
            sessions: sessions.into_iter().map(Into::into).collect(),
        }))
    }

    async fn create(
        &self,
        request: Request<SessionCreateRequest>,
    ) -> Result<Response<SessionCreateResponse>, Status> {
        let req = request.into_inner();
        let session = self
            .handler
            .session_create(&req.name, req.client_request_id.as_deref())
            .await
            .map_err(to_status)?;
        Ok(Response::new(SessionCreateResponse {
            session: Some(session.into()),
        }))
    }

    // ... remaining methods follow the same delegation pattern
}

pub struct EventsService {
    handler: RpcHandler,
}

#[tonic::async_trait]
impl events_server::Events for EventsService {
    type WatchStream = tokio_stream::wrappers::ReceiverStream<
        Result<EventNotification, Status>,
    >;

    async fn watch(
        &self,
        request: Request<EventWatchRequest>,
    ) -> Result<Response<Self::WatchStream>, Status> {
        let req = request.into_inner();
        let (tx, rx) = tokio::sync::mpsc::channel(req.buffer_size.max(64) as usize);

        let handler = self.handler.clone();
        let filters = req.filters;
        let from_seq = req.from_seq;

        tokio::spawn(async move {
            let mut event_rx = handler
                .events_watch(&filters, from_seq)
                .await
                .expect("Failed to start event watch");

            while let Some(event) = event_rx.recv().await {
                let notification = EventNotification {
                    seq: event.seq,
                    timestamp: event.timestamp,
                    event_type: event.event_type,
                    data_json: event.data_json,
                };

                if tx.send(Ok(notification)).await.is_err() {
                    break; // Client disconnected
                }
            }
        });

        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }

    async fn history(
        &self,
        request: Request<EventHistoryRequest>,
    ) -> Result<Response<EventHistoryResponse>, Status> {
        let req = request.into_inner();
        let events = self
            .handler
            .events_history(req.count, &req.filters)
            .await
            .map_err(to_status)?;
        Ok(Response::new(EventHistoryResponse {
            events: events.into_iter().map(Into::into).collect(),
        }))
    }
}

/// Convert an RPC handler error to a tonic Status.
fn to_status(err: crate::handler::RpcError) -> Status {
    match err.code {
        -32600 => Status::invalid_argument(err.message),
        -32601 => Status::not_found(err.message),
        -32001 => Status::failed_precondition(err.message), // version_conflict
        -32602 => Status::invalid_argument(err.message),
        _ => Status::internal(err.message),
    }
}
```

### Step 8: Wire Up gRPC Server

Create `crates/shux-rpc/src/grpc.rs`:

```rust
//! gRPC server module — optional transport alongside JSON-RPC.
//!
//! Enabled via `grpc_enabled = true` in `[daemon]` config.
//! Serves the same operations as JSON-RPC with protobuf-typed streaming.

#[cfg(feature = "grpc")]
pub mod auth;
#[cfg(feature = "grpc")]
pub mod generated;
#[cfg(feature = "grpc")]
pub mod service;
#[cfg(feature = "grpc")]
pub mod uds;

#[cfg(feature = "grpc")]
use crate::handler::RpcHandler;

#[cfg(feature = "grpc")]
use std::path::PathBuf;

#[cfg(feature = "grpc")]
pub struct GrpcConfig {
    pub uds_path: PathBuf,
    pub tcp_addr: Option<String>,
    pub auth_token: String,
}

/// Start the gRPC server (both UDS and optional TCP).
///
/// This function spawns tokio tasks and returns handles.
#[cfg(feature = "grpc")]
pub async fn start_grpc_server(
    handler: RpcHandler,
    config: GrpcConfig,
    shutdown: tokio_util::sync::CancellationToken,
) -> Result<(), Box<dyn std::error::Error>> {
    use service::*;
    use generated::shux::v1::*;

    let system_svc = system_server::SystemServer::new(SystemService {
        handler: handler.clone(),
    });
    let sessions_svc = sessions_server::SessionsServer::new(SessionsService {
        handler: handler.clone(),
    });
    let events_svc = events_server::EventsServer::new(EventsService {
        handler: handler.clone(),
    });

    // Build the tonic router with all services
    let router = tonic::transport::Server::builder()
        .add_service(system_svc)
        .add_service(sessions_svc)
        .add_service(events_svc);
    // ... add remaining services

    // UDS transport (always enabled when gRPC is on)
    let uds_router = router.clone();
    let uds_path = config.uds_path.clone();
    let uds_shutdown = shutdown.clone();
    tokio::spawn(async move {
        tokio::select! {
            result = uds::serve_uds(uds_router, uds_path) => {
                if let Err(e) = result {
                    tracing::error!("gRPC UDS server error: {}", e);
                }
            }
            _ = uds_shutdown.cancelled() => {
                tracing::info!("gRPC UDS server shutting down");
            }
        }
    });

    // TCP transport (optional, requires auth)
    if let Some(tcp_addr) = config.tcp_addr {
        let tcp_shutdown = shutdown.clone();
        let auth_token = config.auth_token.clone();

        let auth_layer = tower::ServiceBuilder::new()
            .layer(tonic::service::interceptor(move |req| {
                auth::check_auth(req, &auth_token)
            }));

        let tcp_router = tonic::transport::Server::builder()
            .layer(auth_layer)
            .add_service(system_server::SystemServer::new(SystemService {
                handler: handler.clone(),
            }));
        // ... add remaining services with auth layer

        let addr = tcp_addr.parse()?;
        tokio::spawn(async move {
            tracing::info!(%tcp_addr, "gRPC TCP server listening");
            tokio::select! {
                result = tcp_router.serve(addr) => {
                    if let Err(e) = result {
                        tracing::error!("gRPC TCP server error: {}", e);
                    }
                }
                _ = tcp_shutdown.cancelled() => {
                    tracing::info!("gRPC TCP server shutting down");
                }
            }
        });
    }

    Ok(())
}
```

### Step 9: Add Feature Flag and Dependencies

Update `crates/shux-rpc/Cargo.toml`:

```toml
[features]
default = []
grpc = ["tonic", "prost", "tonic-build", "tokio-stream"]

[dependencies]
# ... existing deps ...
tonic = { workspace = true, optional = true }
prost = { workspace = true, optional = true }
tokio-stream = { version = "0.1", optional = true, features = ["net"] }

[build-dependencies]
tonic-build = { version = "0.12", optional = true }
```

### Step 10: Add Integration Tests

```rust
#[cfg(all(test, feature = "grpc"))]
mod grpc_tests {
    use super::*;

    #[test]
    fn auth_rejects_missing_token() {
        let req = tonic::Request::new(());
        let result = auth::check_auth(req, "secret-token");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn auth_rejects_wrong_token() {
        let mut req = tonic::Request::new(());
        req.metadata_mut().insert(
            "authorization",
            "Bearer wrong-token".parse().unwrap(),
        );
        let result = auth::check_auth(req, "secret-token");
        assert!(result.is_err());
    }

    #[test]
    fn auth_accepts_correct_token() {
        let mut req = tonic::Request::new(());
        req.metadata_mut().insert(
            "authorization",
            "Bearer secret-token".parse().unwrap(),
        );
        let result = auth::check_auth(req, "secret-token");
        assert!(result.is_ok());
    }

    #[test]
    fn auth_skips_when_empty_token() {
        let req = tonic::Request::new(());
        let result = auth::check_auth(req, "");
        assert!(result.is_ok());
    }

    #[test]
    fn constant_time_eq_works() {
        assert!(auth::constant_time_eq(b"hello", b"hello"));
        assert!(!auth::constant_time_eq(b"hello", b"world"));
        assert!(!auth::constant_time_eq(b"short", b"longer"));
    }

    #[test]
    fn proto_files_exist() {
        let proto_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent().unwrap().parent().unwrap()
            .join("proto/shux/v1");
        assert!(proto_dir.join("shux.proto").exists());
        assert!(proto_dir.join("types.proto").exists());
        assert!(proto_dir.join("events.proto").exists());
    }
}
```

---

## Verification

### Functional

```bash
# Build with grpc feature
cargo build -p shux-rpc --features grpc

# Verify proto files compile
cargo build -p shux-rpc --features grpc 2>&1 | grep -i error
# Expected: no errors

# Start daemon with gRPC enabled
shux config set daemon.grpc_enabled true
# Expected: gRPC UDS socket created alongside JSON-RPC socket

# Test with grpcurl (if installed)
grpcurl -plaintext -unix /tmp/shux-grpc.sock shux.v1.System/Version
# Expected: version info returned

# Test TCP with auth
grpcurl -H "Authorization: Bearer $(cat ~/.config/shux/token)" localhost:50051 shux.v1.System/Health
# Expected: health info returned

# Test event streaming
grpcurl -plaintext -unix /tmp/shux-grpc.sock shux.v1.Events/Watch
# Expected: streaming events
```

### Tests

```bash
# Run gRPC tests
cargo nextest run -p shux-rpc --features grpc

# Expected tests passing:
# - auth_rejects_missing_token
# - auth_rejects_wrong_token
# - auth_accepts_correct_token
# - auth_skips_when_empty_token
# - constant_time_eq_works
# - proto_files_exist
```

---

## Completion Criteria

- [ ] Proto files published in `proto/shux/v1/` (shux.proto, types.proto, events.proto)
- [ ] Every JSON-RPC method has a corresponding gRPC method (1:1 parity)
- [ ] gRPC served over UDS via custom tonic connector
- [ ] gRPC served over TCP with Bearer token auth (same token as JSON-RPC TCP)
- [ ] Auth uses constant-time comparison to prevent timing attacks
- [ ] Event streaming works via gRPC server-side streaming (Events/Watch)
- [ ] gRPC is behind `grpc` feature flag (not compiled by default)
- [ ] Config: `grpc_enabled = false` by default in `[daemon]`
- [ ] UDS socket has 0700 permissions
- [ ] Proto files are self-contained and usable for client codegen in any language
- [ ] gRPC error codes map sensibly to JSON-RPC error codes
- [ ] Graceful shutdown via CancellationToken
- [ ] Unit tests pass for auth, proto existence
- [ ] Integration test: gRPC method returns same data as equivalent JSON-RPC call

---

## Commit Message

```
feat(rpc): add optional gRPC transport with tonic over UDS and TCP

- Proto definitions in proto/shux/v1/ covering all API methods
- UDS connector via serve_with_incoming for local connections
- TCP with Bearer token auth (same token as JSON-RPC TCP)
- Event streaming via gRPC server-side streaming
- Feature-gated behind 'grpc' flag, opt-in via config
- 1:1 parity with JSON-RPC methods, shared handler layer
```

---

## Session Protocol

1. **Before starting:** Read task 035 (JSON-RPC API surface) to understand the complete method list and the `RpcHandler` abstraction that methods delegate to. Read the `tonic` crate documentation for `serve_with_incoming` UDS support. Verify tonic 0.12 API compatibility.
2. **During:** Start with proto file definitions (Steps 1-3), then build.rs (Step 4), then UDS connector (Step 5), auth (Step 6), service impls (Step 7), wiring (Step 8). Compile after each step. Proto files should be reviewed for completeness against SS8.2 method list.
3. **Key design constraint:** The gRPC service must delegate to the same `RpcHandler` used by JSON-RPC. No business logic in gRPC service impls — only type conversion.
4. **Edge cases to watch for:**
   - tonic version compatibility with prost (must match)
   - UDS socket cleanup on shutdown (remove file)
   - Streaming cancellation when client disconnects
   - Large state.snapshot paginated responses via streaming
   - Proto field numbering must be stable (never reuse field numbers)
5. **After:** Run full test suite. Verify proto files compile cleanly with `protoc` independently. Update `docs/PROGRESS.md`. Update `CLAUDE.md` Learnings (create from task 000 template if missing).
