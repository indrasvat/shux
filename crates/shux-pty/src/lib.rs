//! shux-pty — PTY manager with async I/O and process lifecycle.
//!
//! Provides `PtyHandle` (per-pane PTY wrapper) and `PtyManager`
//! (coordinates all PTY handles with lifecycle events).

pub mod capture;
pub mod command;
pub mod handle;
pub mod manager;

pub use capture::strip_ansi;
pub use command::{CommandEngine, CommandResult};
pub use handle::{PtyConfig, PtyError, PtyHandle, PtySize};
pub use manager::{PaneId, PtyEvent, PtyManager};
