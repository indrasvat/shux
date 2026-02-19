//! shux-pty — PTY manager with async I/O and process lifecycle.
//!
//! Provides `PtyHandle` (per-pane PTY wrapper) and `PtyManager`
//! (coordinates all PTY handles with lifecycle events).

pub mod handle;
pub mod manager;

pub use handle::{PtyConfig, PtyError, PtyHandle, PtySize};
pub use manager::{PaneId, PtyEvent, PtyManager};
