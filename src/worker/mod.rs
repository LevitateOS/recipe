//! Worker endpoint root (R01 owns framing; R02 owns the worker engine hookup).
//!
//! The worker protocol v1 framing lives in [`protocol`]. The worker entry
//! point lives in [`run`]: it drives the existing engine through one request
//! frame under the F04 [`ExecutionContext::worker_stub`] seam and terminates
//! the stream with exactly one result frame.

pub mod protocol;
pub mod run;

pub use run::run_worker;
