//! shux-ui -- TUI client: render compositor, terminal management, input handling.
//!
//! This crate provides the terminal user interface for shux clients:
//! input decoding (crossterm events -> shux InputEvents), key types,
//! the render compositor (double-buffered diff-based rendering), and
//! VT-to-render cell conversion.

pub mod buffer;
pub mod compositor;
pub mod input;
pub mod keys;
pub mod render;
pub mod vt_convert;

// Re-export commonly used types.
pub use buffer::{DirtyCell, FrameBuffer, RenderAttrs, RenderCell};
pub use compositor::{CompositorConfig, RenderCompositor, RenderStats};
pub use input::{InputEvent, KeyboardProtocol, MouseAction, MouseButton, MouseEvent};
pub use keys::{KeyPress, KeyValue, Modifiers, NamedKey};
pub use render::RenderBackend;
