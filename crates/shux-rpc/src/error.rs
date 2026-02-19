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
