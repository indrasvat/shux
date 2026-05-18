//! shux-ui -- TUI client: render compositor, terminal management, input handling.
//!
//! This crate provides the terminal user interface for shux clients:
//! input decoding (crossterm events -> shux InputEvents), key types,
//! the render compositor (double-buffered diff-based rendering),
//! VT-to-render cell conversion, terminal state management, and the
//! TUI client event loop.

pub mod attach;
pub mod borders;
pub mod buffer;
pub mod client;
pub mod composed;
pub mod compositor;
pub mod copy_mode;
pub mod help_overlay;
pub mod input;
pub mod keybinding;
pub mod keys;
pub mod render;
pub mod statusbar;
pub mod terminal;
pub mod vt_convert;

// Re-export commonly used types.
pub use borders::{BorderChars, BorderColors, BorderSegment, BorderStyle, compute_borders};
pub use buffer::{DirtyCell, FrameBuffer, RenderAttrs, RenderCell};
pub use client::{ClientConfig, ExitReason};
pub use composed::{ComposeInputs, ComposedFrame, compose};
pub use compositor::{CompositorConfig, MultiPaneFrame, RenderCompositor, RenderStats};
pub use copy_mode::{
    CopyKey, CopyModeState, handle_key as copy_mode_key,
    handle_key_with_vt as copy_mode_key_with_vt, osc52_copy, render_copy_overlay_into,
    render_copy_view_into,
};
pub use help_overlay::render_help_overlay_into;
pub use input::{InputEvent, KeyboardProtocol, MouseAction, MouseButton, MouseEvent};
pub use keybinding::{BindingTarget, KeybindingError, KeybindingRegistry};
pub use keys::{KeyPress, KeyValue, Modifiers, NamedKey};
pub use render::RenderBackend;
pub use statusbar::{StatusBar, StatusSegment};
pub use terminal::TerminalGuard;
