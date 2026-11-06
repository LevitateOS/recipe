//! Core infrastructure for recipe execution
//!
//! Minimal executor that handles ctx-based recipes.

pub(crate) mod autofix;
pub(crate) mod build_deps;
pub(crate) mod events;
pub(crate) mod executor;
pub(crate) mod invocation;
pub(crate) mod output;
pub(crate) mod runner;
