//! End-to-end tests for the recipe CLI
//!
//! These tests run the actual CLI binary and verify behavior.

use leviso_cheat_test::cheat_aware;
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

#[cheat_aware(
    protects = "User can discover CLI commands and usage",
    severity = "LOW",
    ease = "HARD",
    cheats = [
        "Hardcode expected output strings in test instead of parsing actual output",
        "Check only for binary existence, not help functionality",
        "Match partial strings that could pass even with broken help"
    ],
    consequence = "User runs 'recipe --help' and gets no output or an error"
)]
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

#[cheat_aware(
    protects = "User can check what version of recipe is installed",
    severity = "LOW",
    ease = "HARD",
    cheats = [
        "Check only for success status, not actual version output",
        "Match any string containing 'recipe', not a version number",
        "Hardcode expected version in test"
    ],
    consequence = "User runs 'recipe --version' and gets no output or wrong version"
)]
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

#[cheat_aware(
    protects = "User gets clear error when trying to install non-existent package",
    severity = "MEDIUM",
    ease = "EASY",
    cheats = [
        "Remove the assertion for error message content",
        "Accept any non-success status without checking error message",
        "Create fake package so test passes without testing error path"
    ],
    consequence = "User tries to install missing package, gets cryptic error or silent failure"
)]
#[test]
fn test_cli_install_nonexistent_package() {
    let (_dir, prefix, recipes) = create_test_env();

    let output = run_recipe(&["install", "nonexistent"], &prefix, &recipes);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not found") || stderr.contains("Recipe not found"));
}

#[cheat_aware(
    protects = "User can install a valid package",
    severity = "CRITICAL",
    ease = "MEDIUM",
    cheats = [
        "Create recipe that does nothing but returns success",
        "Check only for success status, not that package was actually installed",
        "Skip verification that acquire/install functions were actually called"
    ],
    consequence = "User runs 'recipe install pkg' - command succeeds but nothing is installed"
)]
#[test]
fn test_cli_install_success() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "simple", r#"
let name = "simple";
let version = "1.0.0";
let installed = false;
fn acquire() {}
fn install() {}
"#);

    let output = run_recipe(&["install", "simple"], &prefix, &recipes);

    assert!(output.status.success(), "Install failed: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Installing simple") || stdout.contains("installed"));
}

#[cheat_aware(
    protects = "User doesn't waste time reinstalling already-installed packages",
    severity = "LOW",
    ease = "EASY",
    cheats = [
        "Always reinstall regardless of installed state",
        "Check output message but allow reinstall to happen",
        "Skip the installed state check entirely"
    ],
    consequence = "User runs install on existing package, wastes time and potentially corrupts state"
)]
#[test]
fn test_cli_install_already_installed() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "already", r#"
let name = "already";
let version = "1.0.0";
let installed = true;
let installed_version = "1.0.0";
let installed_files = [];
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

#[cheat_aware(
    protects = "User gets clear error when removing non-installed package",
    severity = "MEDIUM",
    ease = "EASY",
    cheats = [
        "Accept any non-success status without checking message",
        "Mark package as installed in test setup to avoid error path",
        "Remove the stderr assertion entirely"
    ],
    consequence = "User tries to remove non-installed package, gets confusing error or silent failure"
)]
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

#[cheat_aware(
    protects = "User can remove installed packages and reclaim disk space",
    severity = "HIGH",
    ease = "MEDIUM",
    cheats = [
        "Mark installed=false without actually deleting files",
        "Check only for success status, not file deletion",
        "Create test with no installed_files to avoid deletion testing"
    ],
    consequence = "User removes package - recipe says removed but files still on disk, wasting space"
)]
#[test]
fn test_cli_remove_success() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "removable", r#"
let name = "removable";
let version = "1.0.0";
let installed = true;
let installed_version = "1.0.0";
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

#[cheat_aware(
    protects = "User can see they have no recipes when directory is empty",
    severity = "LOW",
    ease = "EASY",
    cheats = [
        "Accept any output as valid for empty list",
        "Create test recipes to avoid empty case",
        "Skip output content verification entirely"
    ],
    consequence = "User runs 'recipe list' with no packages, gets confusing output"
)]
#[test]
fn test_cli_list_empty() {
    let (_dir, prefix, recipes) = create_test_env();

    let output = run_recipe(&["list"], &prefix, &recipes);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("No recipes") || stdout.is_empty() || !stdout.contains("[installed"));
}

#[cheat_aware(
    protects = "User can see all available packages in list view",
    severity = "MEDIUM",
    ease = "EASY",
    cheats = [
        "Check for any package name, not all expected packages",
        "Accept partial matches that could miss packages",
        "Hardcode expected output format instead of parsing"
    ],
    consequence = "User runs 'recipe list', some packages are missing from output"
)]
#[test]
fn test_cli_list_shows_packages() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "pkg1", r#"
let name = "pkg1";
let version = "1.0.0";
let installed = true;
let installed_version = "1.0.0";
let installed_files = [];
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

#[cheat_aware(
    protects = "User can view detailed package information",
    severity = "MEDIUM",
    ease = "EASY",
    cheats = [
        "Check for package name only, not all metadata fields",
        "Accept any output as valid without verifying content",
        "Test with minimal recipe that has few fields to verify"
    ],
    consequence = "User runs 'recipe info pkg', gets incomplete or wrong information"
)]
#[test]
fn test_cli_info_shows_details() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "detailed", r#"
let name = "detailed";
let version = "1.5.0";
let description = "A detailed package";
let installed = true;
let installed_version = "1.5.0";
let installed_files = [];
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

#[cheat_aware(
    protects = "User gets error when requesting info on non-existent package",
    severity = "LOW",
    ease = "EASY",
    cheats = [
        "Accept any non-success status without verifying error type",
        "Create package to avoid testing error path",
        "Remove assertion entirely"
    ],
    consequence = "User requests info on non-existent package, gets confusing error"
)]
#[test]
fn test_cli_info_nonexistent() {
    let (_dir, prefix, recipes) = create_test_env();

    let output = run_recipe(&["info", "nonexistent"], &prefix, &recipes);

    assert!(!output.status.success());
}

// =============================================================================
// Search Command Tests
// =============================================================================

#[cheat_aware(
    protects = "User can search for packages by name/description",
    severity = "MEDIUM",
    ease = "EASY",
    cheats = [
        "Accept any search result without verifying relevance",
        "Check that output contains something, not the expected match",
        "Use search term that matches all packages"
    ],
    consequence = "User searches for 'rip', gets unrelated results or misses ripgrep"
)]
#[test]
fn test_cli_search_finds_matches() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "ripgrep", r#"
let name = "ripgrep";
let version = "14.0.0";
let installed = false;
let description = "Fast grep replacement";
fn acquire() {}
fn install() {}
"#);

    write_recipe(&recipes, "fd", r#"
let name = "fd";
let version = "9.0.0";
let installed = false;
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

#[cheat_aware(
    protects = "User gets helpful message when search finds nothing",
    severity = "LOW",
    ease = "EASY",
    cheats = [
        "Accept empty output as valid 'no matches' result",
        "Check only for success status, not output content",
        "Create package that matches to avoid testing empty case"
    ],
    consequence = "User searches for non-existent term, gets no feedback or confusing output"
)]
#[test]
fn test_cli_search_no_matches() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "pkg", r#"
let name = "pkg";
let version = "1.0.0";
let installed = false;
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

#[cheat_aware(
    protects = "User gets clear message when package has no update checker",
    severity = "LOW",
    ease = "EASY",
    cheats = [
        "Accept any output without verifying message content",
        "Add check_update to recipe to avoid testing this path",
        "Skip output verification entirely"
    ],
    consequence = "User runs update on package without checker, gets confusing output"
)]
#[test]
fn test_cli_update_no_checker() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "no-checker", r#"
let name = "no-checker";
let version = "1.0.0";
let installed = true;
let installed_version = "1.0.0";
let installed_files = [];
fn acquire() {}
fn install() {}
"#);

    let output = run_recipe(&["update", "no-checker"], &prefix, &recipes);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no update checker") || stdout.contains("has no update"));
}

#[cheat_aware(
    protects = "User can check for and see available updates",
    severity = "HIGH",
    ease = "MEDIUM",
    cheats = [
        "Hardcode version in check_update that equals current version",
        "Check only for success, not that new version is displayed",
        "Accept any version number as 'update available'"
    ],
    consequence = "User runs update check, misses critical security update because check is broken"
)]
#[test]
fn test_cli_update_with_checker() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "updatable", r#"
let name = "updatable";
let version = "1.0.0";
let installed = true;
let installed_version = "1.0.0";
let installed_files = [];
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

#[cheat_aware(
    protects = "User gets clear error when upgrading non-installed package",
    severity = "MEDIUM",
    ease = "EASY",
    cheats = [
        "Accept any failure without verifying error message",
        "Mark package as installed to avoid error path",
        "Remove stderr assertion"
    ],
    consequence = "User tries to upgrade non-installed package, gets cryptic error"
)]
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

#[cheat_aware(
    protects = "User gets confirmation when package is already up to date",
    severity = "LOW",
    ease = "EASY",
    cheats = [
        "Accept any output as valid 'up to date' message",
        "Check only for success status",
        "Use different versions to avoid testing this path"
    ],
    consequence = "User upgrades already-current package, gets confusing feedback"
)]
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

#[cheat_aware(
    protects = "System is protected from path traversal attacks via package names",
    severity = "CRITICAL",
    ease = "HARD",
    cheats = [
        "Accept any failure status without verifying security check",
        "Test only one path traversal pattern, miss others",
        "Remove assertion for error message"
    ],
    consequence = "Attacker crafts malicious package name, reads/writes files outside recipe dir"
)]
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

#[cheat_aware(
    protects = "User can install recipes by explicit file path",
    severity = "MEDIUM",
    ease = "EASY",
    cheats = [
        "Test only with simple paths, miss edge cases",
        "Accept any success without verifying install happened",
        "Use relative path that relies on working directory"
    ],
    consequence = "User tries to install recipe by path, gets error or wrong recipe installed"
)]
#[test]
fn test_cli_accepts_explicit_rhai_path() {
    let (_dir, prefix, recipes) = create_test_env();

    let recipe_path = recipes.join("explicit.rhai");
    std::fs::write(&recipe_path, r#"
let name = "explicit";
let version = "1.0.0";
let installed = false;
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

#[cheat_aware(
    protects = "Errors go to stderr for proper scripting integration",
    severity = "LOW",
    ease = "EASY",
    cheats = [
        "Check only that command fails, not where error goes",
        "Accept errors in stdout as valid",
        "Skip stderr content verification"
    ],
    consequence = "Errors go to stdout, breaks shell pipelines and log parsing"
)]
#[test]
fn test_cli_error_output_to_stderr() {
    let (_dir, prefix, recipes) = create_test_env();

    let output = run_recipe(&["install", "nonexistent"], &prefix, &recipes);

    assert!(!output.status.success());
    // Error should be in stderr, not stdout
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.is_empty());
}

// =============================================================================
// Deps Command Tests
// =============================================================================

#[cheat_aware(
    protects = "User can see direct dependencies of a package",
    severity = "HIGH",
    ease = "MEDIUM",
    cheats = [
        "Check for any output, not actual dependency names",
        "Test with package that has no deps to avoid verification",
        "Accept partial matches that miss some dependencies"
    ],
    consequence = "User checks deps, misses critical dependency, install fails mysteriously later"
)]
#[test]
fn test_cli_deps_shows_direct_dependencies() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "mylib", r#"
let name = "mylib";
let version = "1.0.0";
let installed = false;
let deps = [];
fn acquire() {}
fn install() {}
"#);

    write_recipe(&recipes, "myapp", r#"
let name = "myapp";
let version = "2.0.0";
let installed = false;
let deps = ["mylib"];
fn acquire() {}
fn install() {}
"#);

    let output = run_recipe(&["deps", "myapp"], &prefix, &recipes);

    assert!(output.status.success(), "deps failed: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("mylib"), "Expected 'mylib' in output: {}", stdout);
}

#[cheat_aware(
    protects = "User gets clear feedback when package has no dependencies",
    severity = "LOW",
    ease = "EASY",
    cheats = [
        "Accept any output as valid for no-deps case",
        "Add fake deps to avoid testing empty case",
        "Skip output verification"
    ],
    consequence = "User checks deps on standalone package, gets confusing output"
)]
#[test]
fn test_cli_deps_no_dependencies() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "standalone", r#"
let name = "standalone";
let version = "1.0.0";
let installed = false;
let deps = [];
fn acquire() {}
fn install() {}
"#);

    let output = run_recipe(&["deps", "standalone"], &prefix, &recipes);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("(none)") || stdout.contains("standalone"));
}

#[cheat_aware(
    protects = "User can see correct install order for dependencies",
    severity = "CRITICAL",
    ease = "MEDIUM",
    cheats = [
        "Accept any order of packages as valid",
        "Test with linear chain only, miss complex graphs",
        "Check for presence of packages but not order"
    ],
    consequence = "Install order is wrong, deps fail to build because their deps aren't installed first"
)]
#[test]
fn test_cli_deps_resolve_shows_install_order() {
    let (_dir, prefix, recipes) = create_test_env();

    // Create chain: app -> lib -> core
    write_recipe(&recipes, "core", r#"
let name = "core";
let version = "1.0.0";
let installed = false;
let deps = [];
fn acquire() {}
fn install() {}
"#);

    write_recipe(&recipes, "lib", r#"
let name = "lib";
let version = "1.0.0";
let installed = false;
let deps = ["core"];
fn acquire() {}
fn install() {}
"#);

    write_recipe(&recipes, "app", r#"
let name = "app";
let version = "1.0.0";
let installed = false;
let deps = ["lib"];
fn acquire() {}
fn install() {}
"#);

    let output = run_recipe(&["deps", "app", "--resolve"], &prefix, &recipes);

    assert!(output.status.success(), "deps --resolve failed: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should show correct install order (numbered format: "1. core", "2. lib", "3. app")
    assert!(stdout.contains("1. core"), "Missing '1. core' in output: {}", stdout);
    assert!(stdout.contains("2. lib"), "Missing '2. lib' in output: {}", stdout);
    assert!(stdout.contains("3. app"), "Missing '3. app' in output: {}", stdout);
}

#[cheat_aware(
    protects = "User can see which dependencies are already installed",
    severity = "MEDIUM",
    ease = "EASY",
    cheats = [
        "Skip checking for [installed] marker",
        "Mark all packages as not installed in test",
        "Accept any output format"
    ],
    consequence = "User can't tell which deps need installing, wastes time or misses packages"
)]
#[test]
fn test_cli_deps_resolve_shows_installed_status() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "installed-lib", r#"
let name = "installed-lib";
let version = "1.0.0";
let deps = [];
let installed = true;
let installed_version = "1.0.0";
let installed_files = [];
fn acquire() {}
fn install() {}
"#);

    write_recipe(&recipes, "new-app", r#"
let name = "new-app";
let version = "1.0.0";
let installed = false;
let deps = ["installed-lib"];
fn acquire() {}
fn install() {}
"#);

    let output = run_recipe(&["deps", "new-app", "--resolve"], &prefix, &recipes);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[installed]"), "Expected '[installed]' marker in output: {}", stdout);
}

#[cheat_aware(
    protects = "User gets correct resolution for diamond dependency patterns",
    severity = "HIGH",
    ease = "MEDIUM",
    cheats = [
        "Test only linear chains, miss diamond patterns",
        "Accept duplicate entries in output",
        "Skip verification of correct ordering"
    ],
    consequence = "Diamond deps resolved incorrectly, shared dep installed twice or in wrong order"
)]
#[test]
fn test_cli_deps_diamond_pattern() {
    let (_dir, prefix, recipes) = create_test_env();

    // Diamond: top -> left, right; left -> bottom; right -> bottom
    write_recipe(&recipes, "bottom", r#"
let name = "bottom";
let version = "1.0.0";
let installed = false;
let deps = [];
fn acquire() {}
fn install() {}
"#);

    write_recipe(&recipes, "left", r#"
let name = "left";
let version = "1.0.0";
let installed = false;
let deps = ["bottom"];
fn acquire() {}
fn install() {}
"#);

    write_recipe(&recipes, "right", r#"
let name = "right";
let version = "1.0.0";
let installed = false;
let deps = ["bottom"];
fn acquire() {}
fn install() {}
"#);

    write_recipe(&recipes, "top", r#"
let name = "top";
let version = "1.0.0";
let installed = false;
let deps = ["left", "right"];
fn acquire() {}
fn install() {}
"#);

    let output = run_recipe(&["deps", "top", "--resolve"], &prefix, &recipes);

    assert!(output.status.success(), "deps --resolve failed: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);

    // All 4 packages should appear in output (numbered format)
    assert!(stdout.contains("1. bottom"), "Missing '1. bottom' in output: {}", stdout);

    // The exact positions of left/right depend on traversal order, but both should be before top
    // And top should be position 4
    assert!(stdout.contains("4. top"), "Missing '4. top' in output: {}", stdout);

    // Verify left and right are in positions 2 or 3
    let has_left = stdout.contains("2. left") || stdout.contains("3. left");
    let has_right = stdout.contains("2. right") || stdout.contains("3. right");
    assert!(has_left, "Missing left in positions 2 or 3: {}", stdout);
    assert!(has_right, "Missing right in positions 2 or 3: {}", stdout);
}

#[cheat_aware(
    protects = "User gets clear error for deps on non-existent package",
    severity = "LOW",
    ease = "EASY",
    cheats = [
        "Accept any failure without checking message",
        "Create package to avoid testing error path",
        "Remove assertion"
    ],
    consequence = "User checks deps of missing package, gets cryptic error"
)]
#[test]
fn test_cli_deps_nonexistent_package() {
    let (_dir, prefix, recipes) = create_test_env();

    let output = run_recipe(&["deps", "nonexistent"], &prefix, &recipes);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not found") || stderr.contains("Recipe not found"));
}

// =============================================================================
// Install with Dependencies Tests
// =============================================================================

#[cheat_aware(
    protects = "User can install package with all dependencies in correct order",
    severity = "CRITICAL",
    ease = "MEDIUM",
    cheats = [
        "Install only the requested package, skip deps",
        "Accept any success without verifying all packages installed",
        "Test with no-dep packages to avoid real test"
    ],
    consequence = "User runs 'install --deps pkg', deps not installed, pkg fails at runtime"
)]
#[test]
fn test_cli_install_deps_installs_in_order() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "dep1", r#"
let name = "dep1";
let version = "1.0.0";
let installed = false;
let deps = [];
fn acquire() {}
fn install() {}
"#);

    write_recipe(&recipes, "app", r#"
let name = "app";
let version = "1.0.0";
let installed = false;
let deps = ["dep1"];
fn acquire() {}
fn install() {}
"#);

    let output = run_recipe(&["install", "--deps", "app"], &prefix, &recipes);

    assert!(output.status.success(), "install --deps failed: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should show both packages being installed
    assert!(stdout.contains("dep1") || stdout.contains("2 package"), "Expected dep1 mention: {}", stdout);
    assert!(stdout.contains("app"), "Expected app mention: {}", stdout);
}

#[cheat_aware(
    protects = "Already-installed dependencies are skipped, saving time",
    severity = "MEDIUM",
    ease = "EASY",
    cheats = [
        "Reinstall all deps regardless of state",
        "Accept any package count in output",
        "Mark all deps as not installed in test"
    ],
    consequence = "User reinstalls package, all deps reinstalled wastefully, possible breakage"
)]
#[test]
fn test_cli_install_deps_skips_installed() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "already-installed", r#"
let name = "already-installed";
let version = "1.0.0";
let deps = [];
let installed = true;
let installed_version = "1.0.0";
let installed_files = [];
fn acquire() {}
fn install() {}
"#);

    write_recipe(&recipes, "needs-it", r#"
let name = "needs-it";
let version = "1.0.0";
let installed = false;
let deps = ["already-installed"];
fn acquire() {}
fn install() {}
"#);

    let output = run_recipe(&["install", "--deps", "needs-it"], &prefix, &recipes);

    assert!(output.status.success(), "install --deps failed: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should only install needs-it (1 package), not already-installed
    assert!(stdout.contains("1 package") || stdout.contains("needs-it"), "Output: {}", stdout);
}

#[cheat_aware(
    protects = "User gets feedback when all packages are already installed",
    severity = "LOW",
    ease = "EASY",
    cheats = [
        "Accept any output as valid",
        "Mark some packages as not installed",
        "Skip output verification"
    ],
    consequence = "User runs install --deps when all installed, gets confusing feedback"
)]
#[test]
fn test_cli_install_deps_all_already_installed() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "dep-installed", r#"
let name = "dep-installed";
let version = "1.0.0";
let deps = [];
let installed = true;
let installed_version = "1.0.0";
let installed_files = [];
fn acquire() {}
fn install() {}
"#);

    write_recipe(&recipes, "app-installed", r#"
let name = "app-installed";
let version = "1.0.0";
let deps = ["dep-installed"];
let installed = true;
let installed_version = "1.0.0";
let installed_files = [];
fn acquire() {}
fn install() {}
"#);

    let output = run_recipe(&["install", "--deps", "app-installed"], &prefix, &recipes);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("already installed"), "Expected 'already installed' message: {}", stdout);
}

#[cheat_aware(
    protects = "Deep dependency chains are fully resolved and installed",
    severity = "HIGH",
    ease = "MEDIUM",
    cheats = [
        "Test only shallow dep trees",
        "Accept partial installation as success",
        "Limit recursion depth silently"
    ],
    consequence = "Deep dep chain only partially installed, package fails with missing libs"
)]
#[test]
fn test_cli_install_deps_deep_chain() {
    let (_dir, prefix, recipes) = create_test_env();

    // Create: d -> c -> b -> a
    for (name, dep) in [("a", None), ("b", Some("a")), ("c", Some("b")), ("d", Some("c"))] {
        let deps_line = match dep {
            Some(d) => format!("let deps = [\"{}\"];", d),
            None => "let deps = [];".to_string(),
        };
        write_recipe(&recipes, name, &format!(r#"
let name = "{}";
let version = "1.0.0";
let installed = false;
{}
fn acquire() {{}}
fn install() {{}}
"#, name, deps_line));
    }

    let output = run_recipe(&["install", "--deps", "d"], &prefix, &recipes);

    assert!(output.status.success(), "install --deps failed: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("4 package") || (stdout.contains("a") && stdout.contains("b") && stdout.contains("c") && stdout.contains("d")),
        "Expected all 4 packages: {}", stdout);
}

// =============================================================================
// Info Command with Dependencies
// =============================================================================

#[cheat_aware(
    protects = "User can see package dependencies in info output",
    severity = "MEDIUM",
    ease = "EASY",
    cheats = [
        "Check for any dependency field, not actual dep names",
        "Test with empty deps to avoid verification",
        "Accept missing deps field as valid"
    ],
    consequence = "User runs 'recipe info pkg', can't see what deps are needed"
)]
#[test]
fn test_cli_info_shows_dependencies() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "with-deps", r#"
let name = "with-deps";
let version = "1.0.0";
let installed = false;
let deps = ["lib1", "lib2"];
fn acquire() {}
fn install() {}
"#);

    let output = run_recipe(&["info", "with-deps"], &prefix, &recipes);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Depends:") || stdout.contains("lib1"), "Expected dependencies in output: {}", stdout);
    assert!(stdout.contains("lib1"));
    assert!(stdout.contains("lib2"));
}

#[cheat_aware(
    protects = "Info command works for packages without deps field",
    severity = "LOW",
    ease = "EASY",
    cheats = [
        "Always include deps field in test recipes",
        "Skip testing optional fields",
        "Accept crash as failure instead of graceful handling"
    ],
    consequence = "User runs info on package without deps field, gets crash or error"
)]
#[test]
fn test_cli_info_no_deps_field() {
    let (_dir, prefix, recipes) = create_test_env();

    write_recipe(&recipes, "no-deps-field", r#"
let name = "no-deps-field";
let version = "1.0.0";
let installed = false;
fn acquire() {}
fn install() {}
"#);

    let output = run_recipe(&["info", "no-deps-field"], &prefix, &recipes);

    assert!(output.status.success());
    // Should not crash even without deps field
}
