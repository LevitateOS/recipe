//! Worker endpoint root (R01 owns framing; R02 owns the worker engine hookup).
//!
//! The worker protocol v1 framing lives in [`protocol`]. Nothing else lives
//! here yet; R02 will add the worker entry point that drives the existing
//! engine through these frames.

pub mod protocol;
