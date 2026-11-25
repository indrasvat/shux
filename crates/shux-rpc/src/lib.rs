//! shux-rpc — JSON-RPC server for shux.
//!
//! Provides the JSON-RPC 2.0 server over Unix domain socket (always on)
//! and optional TCP loopback. Uses length-prefixed framing (4-byte BE +
//! JSON payload). Includes a method router and error system.

/// The short git SHA this build came from (`unknown` outside a checkout).
pub const GIT_SHA: &str = env!("SHUX_GIT_SHA");

/// This build's version, as daemons and CLIs compare builds. Cargo's, unless
/// `SHUX_VERSION_OVERRIDE` was set at compile time (a skew-test peer build).
pub const VERSION: &str = match option_env!("SHUX_VERSION_OVERRIDE") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

/// Bumped only when the framing, the version handshake or the meaning of an
/// existing method or field changes incompatibly. Builds of one generation
/// serve each other's calls; anything a build lacks fails that call alone.
/// Removing a method or parameter is such a change.
pub const WIRE_GENERATION: u32 = 1;

/// Asks a daemon to retire once its last session ends, or now when it has
/// none: `{"when": "idle"}`. Owner-local.
pub const UPGRADE: &str = "daemon.upgrade";

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
pub use connection::{Transport, current_connection, current_transport, with_transport};
pub use error::{ErrorCode, RpcError};
pub use policy::{Policy, Sensitivity};
pub use router::{Accepts, Handler, Router, RouterBuilder, UNKNOWN_PARAMS};
pub use server::{Server, ServerConfig};
