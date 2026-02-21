//! Integration tests for recipe lifecycle (ctx-based design)
//!
//! These tests verify the ctx-based recipe execution pattern.

use levitate_recipe::RecipeEngine;
use std::path::Path;
use tempfile::TempDir;

/// Create a test environment with build_dir and recipes directories
fn create_test_env() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let build_dir = dir.path().join("build");
    let recipes_dir = dir.path().join("recipes");
    std::fs::create_dir_all(&build_dir).unwrap();
    std::fs::create_dir_all(&recipes_dir).unwrap();
    (dir, build_dir, recipes_dir)
}

/// Write a recipe file and return its path
fn write_recipe(recipes_dir: &Path, name: &str, content: &str) -> std::path::PathBuf {
    let path = recipes_dir.join(format!("{}.rhai", name));
    let mut content = content.to_string();
    // Cleanup is required by repo policy; tests default to a no-op stub.
    if !content.contains("fn cleanup(") {
        content.push_str("\nfn cleanup(ctx, reason) { ctx }\n");
    }
    std::fs::write(&path, content).unwrap();
    path
}

// =============================================================================
// Basic Install Lifecycle Tests

#[path = "integration/extra.rs"]
mod extra;
#[path = "integration/lifecycle.rs"]
mod lifecycle;
