//! Integration tests for recipe helper functions
//!
//! These tests execute example recipes that exercise helper functions.
//! Network-dependent tests are marked with #[ignore] and can be run with:
//!   cargo test -- --ignored

use levitate_recipe::RecipeEngine;
use tempfile::TempDir;

/// Create a test environment with build_dir
fn create_test_env() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let recipes_dir = dir.path().join("recipes");
    let build_dir = dir.path().join("build");
    std::fs::create_dir_all(&recipes_dir).unwrap();
    std::fs::create_dir_all(&build_dir).unwrap();
    (dir, recipes_dir, build_dir)
}

fn write_recipe(path: &std::path::Path, content: &str) {
    let mut content = content.to_string();
    // Cleanup is required by repo policy; helper tests default to a no-op stub.
    if !content.contains("fn cleanup(") {
        content.push_str("\nfn cleanup(ctx, reason) { ctx }\n");
    }
    std::fs::write(path, content).unwrap();
}

// =============================================================================
// Filesystem Helper Tests
// =============================================================================

#[path = "helpers/filesystem.rs"]
mod filesystem;
#[path = "helpers/network.rs"]
mod network;
