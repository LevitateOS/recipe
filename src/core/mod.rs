//! Core infrastructure for recipe execution
//!
//! This module contains the stable infrastructure that powers recipe execution:
//! - Lifecycle orchestration (execute, remove, update, upgrade)
//! - Execution context management
//! - Recipe state persistence
//! - Dependency resolution
//! - Version constraint parsing
//! - Terminal output formatting

mod context;
pub mod deps;
pub(crate) mod lifecycle;
pub mod lockfile;
pub mod output;
pub mod recipe_state;
pub mod version;

// These are used by lifecycle.rs directly and by test code in helpers/install.rs
#[allow(unused_imports)]
pub(crate) use context::{
    CONTEXT, ContextGuard, clear_context, get_installed_files, init_context, record_installed_file,
    with_context, with_context_mut,
};
