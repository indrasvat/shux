//! `shux lens gate` — the declarative scenario runner (task 081).
//!
//! Ownership boundary (design D1): 081 owns runner MECHANICS + RAW SIGNALS; task 082
//! owns status names, `report.json`, the stdout summary, xfail, bless/`--update`, and
//! the exit-code map. The pure, unit-tested core lives here (`scenario`/`env_plan`/
//! `keys`/`compare`/`signal`); the daemon-backed drive loop is `runner`.
//!
//! `vocab`, `cell_compare` and `pixel` are the gate's *vocabulary* — the closed status
//! set and exit map, the cell comparator and golden fingerprint, and the pixel/exact
//! tiers. They arrived here in #151 from `shux-vt` and `shux-raster`, where they had
//! been parked only because a binary-only crate's internals cannot be imported by the
//! frozen contract tests. They depend on those crates' public APIs and neither crate
//! depends on this one, so the direction of the arrow is now the same as everything
//! else in the workspace.

pub mod bless;
pub mod cell_compare;
pub mod compare;
pub mod driver;
pub mod env_plan;
pub mod heat;
pub mod init;
pub mod keys;
pub mod outcome;
pub mod pixel;
pub mod queries;
pub mod review;
pub mod runner;
pub mod scenario;
pub mod secrets;
pub mod signal;
pub mod summary;
pub mod verdict;
pub mod vocab;
