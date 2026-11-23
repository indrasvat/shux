//! shux-rpc — JSON-RPC server for shux.
//!
//! Provides the JSON-RPC 2.0 server over Unix domain socket (always on)
//! and optional TCP loopback. Uses length-prefixed framing (4-byte BE +
//! JSON payload). Includes a method router and error system.

/// The short git SHA this build came from (`unknown` outside a checkout).
pub const GIT_SHA: &str = env!("SHUX_GIT_SHA");

/// Asks a daemon with no sessions to refuse every new one and shut down:
/// `{"retired": bool, "sessions": n}`. Builds before it answer -32601.
pub const RETIRE_IF_IDLE: &str = "daemon.retire_if_idle";

pub mod attach;
pub mod caller;
pub mod codec;
pub mod connection;
pub mod error;
pub mod policy;
pub mod router;
pub mod server;
pub mod uds;

// Re-export key types.
pub use attach::{
    ATTACH_PROTOCOL_VERSION, ActionArgs, ActionKind, AttachClientFrame, AttachHello, AttachReady,
    AttachServerFrame, MouseButton, MouseKind,
};
pub use caller::{current_caller, run_to_completion, with_caller};
pub use codec::{MAX_FRAME_SIZE, create_codec};
pub use connection::current_connection;
pub use error::{ErrorCode, RpcError};
pub use policy::{Policy, Sensitivity};
pub use router::{Handler, Router, RouterBuilder};
pub use server::{Server, ServerConfig};
