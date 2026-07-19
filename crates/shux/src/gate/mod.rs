//! `shux lens gate` — the declarative scenario runner (task 081).
//!
//! Ownership boundary (design D1): 081 owns runner MECHANICS + RAW SIGNALS; task 082
//! owns status names, `report.json`, the stdout summary, xfail, bless/`--update`, and
//! the exit-code map. The pure, unit-tested core lives here (`scenario`/`env_plan`/
//! `keys`/`compare`/`signal`); the daemon-backed drive loop is `runner`.

pub mod bless;
pub mod compare;
pub mod driver;
pub mod env_plan;
pub mod init;
pub mod keys;
pub mod outcome;
pub mod queries;
pub mod review;
pub mod runner;
pub mod scenario;
pub mod secrets;
pub mod signal;
pub mod summary;
pub mod verdict;
