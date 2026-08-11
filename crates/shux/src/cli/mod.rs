//! CLI argument definitions and subcommand handlers.
//!
//! Every `shux` subcommand is a thin wrapper over a JSON-RPC call to the daemon
//! (PRD §4.3 invariant 2: "CLI == API"). The clap definitions live in [`args`],
//! the shared plumbing in [`rpc`] and [`resolve`], and one module per noun holds
//! the handlers that noun's subcommands dispatch to.

pub mod args;
pub mod config;
pub mod events;
pub mod help;
pub mod lens;
pub mod pane;
pub mod plugin;
pub mod resolve;
pub mod rpc;
pub mod session;
pub mod state;
pub mod system;
pub mod window;

#[cfg(test)]
mod test_support;

// The handler surface is flat to its callers: `cli::handle_pane_split`, not
// `cli::pane::handle_pane_split`. Splitting the file did not change the API.
pub use args::*;
pub use config::*;
pub use events::*;
pub use help::*;
pub use lens::*;
pub use pane::*;
pub use plugin::*;
pub use rpc::*;
pub use session::*;
pub use state::*;
pub use system::*;
pub use window::*;
