//! End-to-end tests for the recipe CLI
//!
//! These tests run the actual CLI binary and verify behavior.

use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

/// Get the path to the recipe binary
fn recipe_bin() -> std::path::PathBuf {
    // During tests, the binary is in target/debug/
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // Remove test binary name
    path.pop(); // Remove deps
    path.push("recipe");
    path
}

/// Create a test environment
fn create_test_env() -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let recipes = dir.path().join("recipes");
    std::fs::create_dir_all(&recipes).unwrap();
    (dir, recipes)
}

/// Write a recipe file
fn write_recipe(recipes_dir: &Path, name: &str, content: &str) {
    let path = recipes_dir.join(format!("{}.rhai", name));
    let mut content = content.to_string();
    // Cleanup is required by repo policy; tests default to a no-op stub.
    if !content.contains("fn cleanup(") {
        content.push_str("\nfn cleanup(ctx, reason) { ctx }\n");
    }
    std::fs::write(&path, content).unwrap();
}

/// Run recipe CLI with arguments
fn run_recipe(args: &[&str], recipes: &Path) -> std::process::Output {
    Command::new(recipe_bin())
        .args(args)
        .args(["--recipes-path", recipes.to_str().unwrap()])
        .output()
        .expect("Failed to execute recipe command")
}

// =============================================================================
// CLI Help and Version Tests

#[path = "e2e/cli.rs"]
mod cli;
#[path = "e2e/failures.rs"]
mod failures;
#[path = "e2e/query.rs"]
mod query;
