//! Core infrastructure for recipe execution
//!
//! Minimal executor that handles ctx-based recipes.

mod autofix;
pub mod build_deps;
pub mod events;
pub mod executor;
pub(crate) mod invocation;
pub mod output;
pub mod runner;
