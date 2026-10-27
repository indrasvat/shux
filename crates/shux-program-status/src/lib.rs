//! shux-program-status — the OSC 7501 Program Status Protocol, v0.4.
//!
//! Spec: <https://superlogical.com/rex/docs/build/program-status>.
//!
//! Pure protocol logic with no I/O: [`wire`] decodes a payload into a
//! [`Message`], [`StatusTable`] holds one terminal's records and applies the
//! spec's lifetimes, and [`StatusSummary`] is the small ranked view the rest
//! of shux passes around. Every other crate reaches the protocol through
//! these types only.

pub mod display;
pub mod limits;
mod model;
mod rank;
mod summary;
mod table;
pub mod wire;

pub use model::{Kind, Record, RecordId, RecordView, Report, State};
pub use summary::{StateCounts, StatusSummary};
pub use table::StatusTable;
pub use wire::{Message, Reject};
