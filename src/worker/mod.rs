//! In-domain worker endpoint.
//!
//! The coordinator/worker framing lives in [`protocol`]. The worker entry
//! point lives in [`run`]: it reads one request frame, runs the engine,
//! relays bounded human diagnostics and terminates the stream with exactly
//! one result frame.

pub mod protocol;
mod run;

pub use run::run_worker;
