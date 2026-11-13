//! End-to-end tests for the recipe CLI
//!
//! These tests run the actual CLI binary and verify behavior.

use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

/// Get the path to the recipe binary
fn recipe_bin() -> std::path::PathBuf {
    std::env::var_os("RECIPE_TEST_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_recipe").into())
}

/// Select the exact producer recipe inputs for standalone retained runs.
fn production_recipes() -> std::path::PathBuf {
    std::env::var_os("RECIPE_TEST_BUILDER_RECIPES")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../distro/builder/os-recipes")
        })
}

/// Create a test environment
fn create_test_env() -> (TempDir, std::path::PathBuf) {
    let retained = std::env::var_os("RECIPE_TEST_ARTIFACTS");
    let mut builder = tempfile::Builder::new();
    builder.prefix("recipe e2e ");
    let mut dir = match &retained {
        Some(parent) => builder.tempdir_in(parent),
        None => builder.tempdir(),
    }
    .unwrap();
    dir.disable_cleanup(retained.is_some());
    let recipes = dir.path().join("recipes");
    std::fs::create_dir_all(&recipes).unwrap();
    (dir, recipes)
}

/// Write a recipe file
fn write_recipe(recipes_dir: &Path, name: &str, content: &str) {
    let path = recipes_dir.join(format!("{}.rhai", name));
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

/// The CLI in Ginux mode: these suites exercise the Ginux domain (their
/// recipes spell domain paths such as `/out` literally). Host runs are
/// covered by [`host_command`].
fn recipe_command(recipes: &Path) -> Command {
    let mut command = host_command(recipes);
    command.args(["--execution", "ginux"]);
    command
}

/// The CLI with the default `--execution auto`: a recipe that starts no
/// program runs on the host.
fn host_command(recipes: &Path) -> Command {
    let root = recipes
        .parent()
        .expect("recipes directory has a fixture root");
    let mut command = Command::new(recipe_bin());
    command
        .current_dir(root)
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
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

#[path = "e2e/archives.rs"]
mod archives;
#[path = "e2e/cli.rs"]
mod cli;
#[path = "e2e/command_options.rs"]
mod command_options;
#[path = "e2e/downloads.rs"]
mod downloads;
#[path = "e2e/failures.rs"]
mod failures;
#[path = "e2e/git_acquisition.rs"]
mod git_acquisition;
#[path = "e2e/invocation.rs"]
mod invocation;
#[path = "e2e/kernel_acquisition.rs"]
mod kernel_acquisition;
#[path = "e2e/kernel_recipes.rs"]
mod kernel_recipes;
#[path = "e2e/preseed_recipes.rs"]
mod preseed_recipes;
#[path = "e2e/process.rs"]
mod process;
#[path = "e2e/query.rs"]
mod query;

#[path = "e2e/api2.rs"]
mod api2;

#[path = "e2e/cleanup.rs"]
mod cleanup;

#[path = "e2e/dependencies.rs"]
mod dependencies;

#[cfg(target_os = "linux")]
#[path = "e2e/managed_inputs.rs"]
mod managed_inputs;

#[path = "e2e/worker_entry.rs"]
mod worker_entry;

#[path = "e2e/managed_state.rs"]
mod managed_state;

#[cfg(target_os = "linux")]
#[path = "e2e/managed_target.rs"]
mod managed_target;

#[path = "e2e/managed_storage.rs"]
mod managed_storage;

#[path = "e2e/managed_cancellation.rs"]
mod managed_cancellation;

#[path = "e2e/managed_retention.rs"]
mod managed_retention;

#[path = "e2e/managed_autofix.rs"]
mod managed_autofix;

#[path = "e2e/target_metadata.rs"]
mod target_metadata;

#[path = "e2e/needs.rs"]
mod needs;
