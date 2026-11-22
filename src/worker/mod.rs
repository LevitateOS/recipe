//! In-domain worker endpoint.
//!
//! The coordinator/worker records live in [`records`]. The worker entry
//! point lives in [`run`]: it reads the request file, runs the engine,
//! writes bounded human diagnostics and progress as lines on its standard
//! output, and ends with exactly one result file.

pub mod records;
mod run;

pub use run::run_worker;
