//! Core infrastructure for recipe execution: the engine, how it calls a
//! recipe's functions, saved values, events and output.

pub(crate) mod events;
pub(crate) mod executor;
pub(crate) mod invocation;
pub(crate) mod output;
pub(crate) mod saved;
