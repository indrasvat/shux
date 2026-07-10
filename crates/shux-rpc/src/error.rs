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
    /// Name conflict (session/window name already exists).
    NameConflict,
    /// A `pane.diff_since` `since_revision` has no live checkpoint and is not
    /// covered by an invalidation marker (lens PRD §7.1, LENS-R-033). Carries
    /// `{requested, available:[u64]}` so the caller can re-checkpoint.
    StaleRevision,
    /// A `pane.diff_since` `since_revision` predates a resize / alt-screen
    /// switch that invalidated every checkpoint of that pane (lens PRD §7.1,
    /// DEC-4, LENS-R-032/033).
    ResizeInvalidated,
    /// Response payload exceeds a method's declared size cap (lens PRD §5.2,
    /// LENS-R-*: `pane.glance`'s 8 MiB decoded-PNG cap and friends).
    PayloadTooLarge,
    /// A bounded daemon resource is exhausted (lens PRD §8, LENS-R-043: the
    /// 16-concurrent-scratch-session quota). Not a param error — the request
    /// was well-formed, the daemon just has no room right now.
    ResourceExhausted,
    /// `lens.run`'s synchronous spawn failed (lens PRD §8, LENS-R-040/045):
    /// `argv[0]` did not resolve via PATH, `cwd` was invalid, or exec itself
    /// failed. The scratch allocation is rolled back completely — no
    /// session/pane/PTY survives a SPAWN_FAILED.
    SpawnFailed,
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
            ErrorCode::NameConflict => -32007,
            ErrorCode::StaleRevision => -32010,
            ErrorCode::ResizeInvalidated => -32011,
            ErrorCode::PayloadTooLarge => -32013,
            ErrorCode::ResourceExhausted => -32012,
            ErrorCode::SpawnFailed => -32014,
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
            ErrorCode::NameConflict => "name_conflict",
            ErrorCode::StaleRevision => "stale_revision",
            ErrorCode::ResizeInvalidated => "resize_invalidated",
            ErrorCode::PayloadTooLarge => "payload_too_large",
            ErrorCode::ResourceExhausted => "resource_exhausted",
            ErrorCode::SpawnFailed => "spawn_failed",
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
    pub fn with_message_and_data(code: ErrorCode, message: impl Into<String>, data: Value) -> Self {
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
    pub fn version_conflict(resource: &str, id: &str, expected: u64, actual: u64) -> Self {
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

    /// Name conflict error (duplicate name).
    pub fn name_conflict(resource: &str, name: &str) -> Self {
        Self::with_data(
            ErrorCode::NameConflict,
            serde_json::json!({
                "resource": resource,
                "name": name,
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

    /// Stale-revision error (lens PRD §7.1, LENS-R-033): `pane.diff_since`'s
    /// `since_revision` has no live checkpoint and no invalidation covers it.
    /// `available` is the pane's live checkpoint revisions, sorted ascending,
    /// so the caller knows exactly which revisions are still diffable.
    pub fn stale_revision(requested: u64, available: &[u64]) -> Self {
        Self::with_data(
            ErrorCode::StaleRevision,
            serde_json::json!({
                "requested": requested,
                "available": available,
            }),
        )
    }

    /// Resize-invalidated error (lens PRD §7.1, DEC-4, LENS-R-032/033): the
    /// `since_revision` predates a resize / alt-screen switch that freed every
    /// checkpoint of the pane. `invalidated_at` is the post-mutation revision
    /// of the invalidating event.
    pub fn resize_invalidated(requested: u64, invalidated_at: u64) -> Self {
        Self::with_data(
            ErrorCode::ResizeInvalidated,
            serde_json::json!({
                "requested": requested,
                "invalidated_at": invalidated_at,
                "hint": "re-checkpoint the pane after the resize / alt-screen switch",
            }),
        )
    }

    /// Payload too large error (lens PRD §5.2: `pane.glance`'s 8 MiB
    /// decoded-PNG cap). `size`/`max_size` are byte counts of the
    /// oversized payload, not the base64 encoding of it.
    pub fn payload_too_large(size: usize, max: usize) -> Self {
        Self::with_data(
            ErrorCode::PayloadTooLarge,
            serde_json::json!({
                "size": size,
                "max_size": max,
                "hint": "shrink the pane (pane.set_size) or set include_png=false"
            }),
        )
    }

    /// Resource-exhausted error (lens PRD §8, LENS-R-043): `lens.run`'s
    /// 16-concurrent-scratch-session quota is full. `current`/`max` let the
    /// caller decide whether to retry after killing one of its own scratch
    /// sessions.
    pub fn resource_exhausted(resource: &str, current: usize, max: usize) -> Self {
        Self::with_data(
            ErrorCode::ResourceExhausted,
            serde_json::json!({
                "resource": resource,
                "current": current,
                "max": max,
                "hint": "kill an existing scratch session (session.kill) and retry",
            }),
        )
    }

    /// Spawn-failed error (lens PRD §8, LENS-R-040/045): `lens.run`'s
    /// synchronous PTY spawn failed (argv[0] not found via PATH, invalid
    /// cwd, or exec error). `detail` carries the underlying OS error; the
    /// caller's scratch allocation is guaranteed rolled back (no session,
    /// pane, or PTY survives).
    pub fn spawn_failed(detail: &str) -> Self {
        Self::with_data(
            ErrorCode::SpawnFailed,
            serde_json::json!({
                "detail": detail,
                "hint": "check argv[0] resolves via PATH and cwd exists",
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

    /// Sorted key list of an error's `data` object (wire-shape pinning).
    fn data_keys(err: &RpcError) -> Vec<String> {
        let json = serde_json::to_value(err).unwrap();
        let mut keys: Vec<String> = json["data"]
            .as_object()
            .expect("error data is an object")
            .keys()
            .cloned()
            .collect();
        keys.sort_unstable();
        keys
    }

    #[test]
    fn test_stale_revision_error() {
        // lens PRD §7.1 LENS-R-033: -32010 with {requested, available:[u64]}.
        let err = RpcError::stale_revision(7, &[3, 5, 6]);
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["code"], -32010);
        assert_eq!(json["message"], "stale_revision");
        assert_eq!(json["data"]["requested"], 7);
        assert_eq!(json["data"]["available"], serde_json::json!([3, 5, 6]));
        // Wire-shape pin (codex P4 convergence major, adjudicated — PRD §7.3):
        // EXACTLY these fields, nothing more, nothing fewer.
        assert_eq!(data_keys(&err), vec!["available", "requested"]);
    }

    #[test]
    fn test_resize_invalidated_error() {
        // lens PRD §7.1 DEC-4: -32011 for a since_revision predating a
        // resize / alt-screen switch.
        let err = RpcError::resize_invalidated(4, 9);
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["code"], -32011);
        assert_eq!(json["message"], "resize_invalidated");
        assert_eq!(json["data"]["requested"], 4);
        assert_eq!(json["data"]["invalidated_at"], 9);
        // Wire-shape pin (codex P4 convergence major, adjudicated — PRD §7.3
        // amended to document the richer agent-first payload): EXACTLY
        // {requested, invalidated_at, hint}.
        assert_eq!(data_keys(&err), vec!["hint", "invalidated_at", "requested"]);
    }

    #[test]
    fn test_payload_too_large_error() {
        let err = RpcError::payload_too_large(9_000_000, 8 * 1024 * 1024);
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["code"], -32013);
        assert_eq!(json["message"], "payload_too_large");
        assert_eq!(json["data"]["size"], 9_000_000);
        assert_eq!(json["data"]["max_size"], 8 * 1024 * 1024);
    }

    #[test]
    fn test_resource_exhausted_error() {
        // lens PRD §8 LENS-R-043: 17th scratch session -> -32012.
        let err = RpcError::resource_exhausted("scratch_session", 16, 16);
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["code"], -32012);
        assert_eq!(json["message"], "resource_exhausted");
        assert_eq!(json["data"]["resource"], "scratch_session");
        assert_eq!(json["data"]["current"], 16);
        assert_eq!(json["data"]["max"], 16);
    }

    #[test]
    fn test_spawn_failed_error() {
        // lens PRD §8 LENS-R-040/045: argv[0] not found -> -32014.
        let err = RpcError::spawn_failed("No such file or directory (os error 2)");
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["code"], -32014);
        assert_eq!(json["message"], "spawn_failed");
        assert!(
            json["data"]["detail"]
                .as_str()
                .unwrap()
                .contains("No such file")
        );
    }
}
