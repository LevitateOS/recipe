//! Core infrastructure for recipe execution
//!
//! This module contains the stable infrastructure that powers recipe execution:
//! - Lifecycle orchestration (execute, remove, update, upgrade)
//! - Execution context management
//! - Recipe state persistence
//! - Dependency resolution
//! - Terminal output formatting

mod context;
pub mod deps;
pub(crate) mod lifecycle;
pub mod output;
pub mod recipe_state;

pub(crate) use context::{
    clear_context, get_installed_files, init_context_with_recipe, record_installed_file,
    with_context, with_context_mut, ContextGuard, CONTEXT,
};
