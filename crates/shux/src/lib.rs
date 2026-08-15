//! shux — internal library target for the `shux` binary.
//!
//! **This library is internal.** The crate ships as a release binary and is not
//! published to crates.io, so nothing here is a stability promise and no
//! external API is being designed. The library exists so that the crate's own
//! integration tests in `crates/shux/tests/` can reach the crate's own code —
//! which a `[[bin]]`-only package cannot offer, because a binary's internals
//! are unreachable from outside it.
//!
//! That constraint was not theoretical. It had already bent the dependency
//! graph: the lens-gate vocabulary — the closed status set, the exit-code map,
//! the `report.json` schema, the cell comparator, the pixel tier — was pushed
//! two layers down into `shux-vt` and `shux-raster` purely so the frozen
//! contract tests could import it. Both files said so in their own placement
//! notes. This target removes the reason; [`gate::vocab`], [`gate::cell_compare`]
//! and [`gate::pixel`] are where that vocabulary lives now.
//!
//! **`main.rs` declares no modules.** A module declared in both a `lib.rs` and
//! a `main.rs` is compiled into each target as two unrelated types with the
//! same name — it builds, and every error after it is baffling. The module
//! tree is owned here and nowhere else; `scripts/check-no-bin-mods.sh` keeps it
//! that way.

pub mod attach;
pub mod cli;
pub mod client;
pub mod config_validate;
pub mod daemon;
pub mod daemon_boot;
pub mod dispatch;
pub mod features;
pub mod gate;
pub mod lens_render;
pub mod lens_scratch;
pub mod onboarding;
pub mod pane_command;
pub mod pane_io;
pub mod pane_record;
pub mod pane_spawn;
pub mod rpc;
pub mod session_meta;
pub mod session_persist;
pub mod settle;
pub mod snapshot;
pub mod statusbar_build;
pub mod statusbar_runner;
pub mod style;
pub mod template;
