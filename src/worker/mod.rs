//! In-domain worker endpoint.
//!
//! The worker protocol v6 framing lives in [`protocol`]. The worker entry
//! point lives in [`run`]: it drives the existing engine through one request
//! frame, relays bounded human diagnostics, runs under the worker execution context and terminates
//! the stream with exactly one result frame.

pub mod protocol;
pub mod run;

pub use run::run_worker;

/// Private argv marker supplied only to the staged worker by the coordinator.
#[doc(hidden)]
pub const WORKER_ENTRY_ARG: &str = "--__recipe-worker";
