//! End-to-end tests for the recipe CLI
//!
//! These tests run the actual CLI binary and verify behavior.

use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

/// Get the path to the recipe binary
fn recipe_bin() -> std::path::PathBuf {
    env!("CARGO_BIN_EXE_recipe").into()
}

/// Create a test environment
fn create_test_env() -> (TempDir, std::path::PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix("recipe e2e ")
        .tempdir()
        .unwrap();
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
    recipe_command(recipes)
        .args(args)
        .arg("--recipes-path")
        .arg(recipes)
        .arg("--build-dir")
        .arg(recipes.parent().unwrap().join("build"))
        .output()
        .expect("Failed to execute recipe command")
}

fn recipe_command(recipes: &Path) -> Command {
    let root = recipes
        .parent()
        .expect("recipes directory has a fixture root");
    let mut command = Command::new(recipe_bin());
    command
        .current_dir(root)
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_CACHE_HOME", root.join("cache"));
    command
}

fn assert_success(output: &std::process::Output) {
    assert!(
        output.status.success(),
        "recipe failed: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

// =============================================================================
// CLI Help and Version Tests

#[path = "e2e/cli.rs"]
mod cli;
#[path = "e2e/failures.rs"]
mod failures;
#[path = "e2e/query.rs"]
mod query;
#[path = "e2e/archives.rs"]
mod archives;
#[path = "e2e/kernel_recipes.rs"]
mod kernel_recipes;
#[path = "e2e/downloads.rs"]
mod downloads;
#[path = "e2e/process.rs"]
mod process;
#[path = "e2e/preseed_recipes.rs"]
mod preseed_recipes;
#[path = "e2e/command_options.rs"]
mod command_options;
#[path = "e2e/git_acquisition.rs"]
mod git_acquisition;
