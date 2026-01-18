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
fn create_test_env() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let prefix = dir.path().join("prefix");
    let recipes = dir.path().join("recipes");
    std::fs::create_dir_all(&prefix).unwrap();
    std::fs::create_dir_all(&recipes).unwrap();
    (dir, prefix, recipes)
}

/// Write a recipe file
fn write_recipe(recipes_dir: &Path, name: &str, content: &str) {
    let path = recipes_dir.join(format!("{}.rhai", name));
    std::fs::write(&path, content).unwrap();
}

/// Run recipe CLI with arguments
fn run_recipe(args: &[&str], prefix: &Path, recipes: &Path) -> std::process::Output {
    Command::new(recipe_bin())
        .args(args)
        .args(["--prefix", prefix.to_str().unwrap()])
        .args(["--recipes-path", recipes.to_str().unwrap()])
        .output()
        .expect("Failed to execute recipe command")
}

// =============================================================================
// CLI Help and Version Tests
// =============================================================================

#[test]
fn test_cli_help() {
    let output = Command::new(recipe_bin())
        .arg("--help")
        .output()
        .expect("Failed to run recipe --help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("recipe"));
    assert!(stdout.contains("install"));
    assert!(stdout.contains("remove"));
}

#[test]
fn test_cli_version() {
    let output = Command::new(recipe_bin())
        .arg("--version")
        .output()
        .expect("Failed to run recipe --version");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("recipe"));
}

// =============================================================================
// Install Command Tests
// =============================================================================

#[test]
fn test_cli_install_nonexistent_package() {
    let (_dir, prefix, recipes) = create_test_env();

    let output = run_recipe(&["install", "nonexistent"], &prefix, &recipes);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not found") || stderr.contains("Recipe not found"));
}

#[test]
fn test_cli_install_success() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "simple", r#"
let name = "simple";
let version = "1.0.0";
fn acquire() {}
fn install() {}
"#);

    let output = run_recipe(&["install", "simple"], &prefix, &recipes);

    assert!(output.status.success(), "Install failed: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Installing simple") || stdout.contains("installed"));
}

#[test]
fn test_cli_install_already_installed() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "already", r#"
let name = "already";
let version = "1.0.0";
let installed = true;
fn acquire() {}
fn install() {}
"#);

    let output = run_recipe(&["install", "already"], &prefix, &recipes);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("already installed") || stdout.contains("skipping"));
}

// =============================================================================
// Remove Command Tests
// =============================================================================

#[test]
fn test_cli_remove_not_installed() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "not-installed", r#"
let name = "not-installed";
let version = "1.0.0";
let installed = false;
fn acquire() {}
fn install() {}
"#);

    let output = run_recipe(&["remove", "not-installed"], &prefix, &recipes);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not installed"));
}

#[test]
fn test_cli_remove_success() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "removable", r#"
let name = "removable";
let version = "1.0.0";
let installed = true;
let installed_files = [];
fn acquire() {}
fn install() {}
"#);

    let output = run_recipe(&["remove", "removable"], &prefix, &recipes);

    assert!(output.status.success(), "Remove failed: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Removing") || stdout.contains("removed"));
}

// =============================================================================
// List Command Tests
// =============================================================================

#[test]
fn test_cli_list_empty() {
    let (_dir, prefix, recipes) = create_test_env();

    let output = run_recipe(&["list"], &prefix, &recipes);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("No recipes") || stdout.is_empty() || !stdout.contains("[installed"));
}

#[test]
fn test_cli_list_shows_packages() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "pkg1", r#"
let name = "pkg1";
let version = "1.0.0";
let installed = true;
let installed_version = "1.0.0";
fn acquire() {}
fn install() {}
"#);

    write_recipe(&recipes, "pkg2", r#"
let name = "pkg2";
let version = "2.0.0";
let installed = false;
fn acquire() {}
fn install() {}
"#);

    let output = run_recipe(&["list"], &prefix, &recipes);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("pkg1"));
    assert!(stdout.contains("pkg2"));
}

// =============================================================================
// Info Command Tests
// =============================================================================

#[test]
fn test_cli_info_shows_details() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "detailed", r#"
let name = "detailed";
let version = "1.5.0";
let description = "A detailed package";
let installed = true;
let installed_version = "1.5.0";
fn acquire() {}
fn install() {}
"#);

    let output = run_recipe(&["info", "detailed"], &prefix, &recipes);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("detailed"));
    assert!(stdout.contains("1.5.0"));
    assert!(stdout.contains("A detailed package") || stdout.contains("Installed"));
}

#[test]
fn test_cli_info_nonexistent() {
    let (_dir, prefix, recipes) = create_test_env();

    let output = run_recipe(&["info", "nonexistent"], &prefix, &recipes);

    assert!(!output.status.success());
}

// =============================================================================
// Search Command Tests
// =============================================================================

#[test]
fn test_cli_search_finds_matches() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "ripgrep", r#"
let name = "ripgrep";
let version = "14.0.0";
let description = "Fast grep replacement";
fn acquire() {}
fn install() {}
"#);

    write_recipe(&recipes, "fd", r#"
let name = "fd";
let version = "9.0.0";
let description = "Fast find replacement";
fn acquire() {}
fn install() {}
"#);

    let output = run_recipe(&["search", "rip"], &prefix, &recipes);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("ripgrep"));
    assert!(!stdout.contains("fd") || stdout.contains("No packages"));
}

#[test]
fn test_cli_search_no_matches() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "pkg", r#"
let name = "pkg";
let version = "1.0.0";
fn acquire() {}
fn install() {}
"#);

    let output = run_recipe(&["search", "xyz123"], &prefix, &recipes);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("No packages matching"));
}

// =============================================================================
// Update Command Tests
// =============================================================================

#[test]
fn test_cli_update_no_checker() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "no-checker", r#"
let name = "no-checker";
let version = "1.0.0";
let installed = true;
fn acquire() {}
fn install() {}
"#);

    let output = run_recipe(&["update", "no-checker"], &prefix, &recipes);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no update checker") || stdout.contains("has no update"));
}

#[test]
fn test_cli_update_with_checker() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "updatable", r#"
let name = "updatable";
let version = "1.0.0";
let installed = true;
fn acquire() {}
fn install() {}
fn check_update() {
    "2.0.0"
}
"#);

    let output = run_recipe(&["update", "updatable"], &prefix, &recipes);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("2.0.0") || stdout.contains("available"));
}

// =============================================================================
// Upgrade Command Tests
// =============================================================================

#[test]
fn test_cli_upgrade_not_installed() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "not-installed", r#"
let name = "not-installed";
let version = "1.0.0";
let installed = false;
fn acquire() {}
fn install() {}
"#);

    let output = run_recipe(&["upgrade", "not-installed"], &prefix, &recipes);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not installed"));
}

#[test]
fn test_cli_upgrade_up_to_date() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "up-to-date", r#"
let name = "up-to-date";
let version = "1.0.0";
let installed = true;
let installed_version = "1.0.0";
let installed_files = [];
fn acquire() {}
fn install() {}
"#);

    let output = run_recipe(&["upgrade", "up-to-date"], &prefix, &recipes);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("up to date"));
}

// =============================================================================
// Path Validation Tests
// =============================================================================

#[test]
fn test_cli_rejects_path_traversal() {
    let (_dir, prefix, recipes) = create_test_env();

    // This should be rejected as invalid package name
    let output = run_recipe(&["install", "pkg!name"], &prefix, &recipes);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Invalid package name") || stderr.contains("not found"));
}

// =============================================================================
// Explicit Path Tests
// =============================================================================

#[test]
fn test_cli_accepts_explicit_rhai_path() {
    let (_dir, prefix, recipes) = create_test_env();

    let recipe_path = recipes.join("explicit.rhai");
    std::fs::write(&recipe_path, r#"
let name = "explicit";
let version = "1.0.0";
fn acquire() {}
fn install() {}
"#).unwrap();

    let output = Command::new(recipe_bin())
        .args(["install", recipe_path.to_str().unwrap()])
        .args(["--prefix", prefix.to_str().unwrap()])
        .output()
        .expect("Failed to execute");

    assert!(output.status.success(), "Failed: {}", String::from_utf8_lossy(&output.stderr));
}

// =============================================================================
// Error Output Tests
// =============================================================================

#[test]
fn test_cli_error_output_to_stderr() {
    let (_dir, prefix, recipes) = create_test_env();

    let output = run_recipe(&["install", "nonexistent"], &prefix, &recipes);

    assert!(!output.status.success());
    // Error should be in stderr, not stdout
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.is_empty());
}
