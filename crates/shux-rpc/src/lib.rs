//! shux-rpc — JSON-RPC server for shux.
//!
//! Provides the JSON-RPC 2.0 server over Unix domain socket (always on)
//! and optional TCP loopback. Uses length-prefixed framing (4-byte BE +
//! JSON payload). Includes a method router and error system.

pub mod attach;
pub mod caller;
pub mod codec;
pub mod error;
pub mod policy;
pub mod router;
pub mod server;

// Re-export key types.
pub use attach::{
    ATTACH_PROTOCOL_VERSION, ActionArgs, ActionKind, AttachClientFrame, AttachHello, AttachReady,
    AttachServerFrame, MouseButton, MouseKind,
};
pub use caller::{current_caller, with_caller};
pub use codec::{MAX_FRAME_SIZE, create_codec};
pub use error::{ErrorCode, RpcError};
pub use policy::{Policy, Sensitivity};
pub use router::{Handler, Router, RouterBuilder};
pub use server::{Server, ServerConfig};
