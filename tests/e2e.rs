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

#[cheat_aware(
    protects = "Nonexistent package produces clear error, not silent success",
    severity = "HIGH",
    ease = "EASY",
    cheats = ["Return success for missing packages", "Create empty package on the fly"],
    consequence = "User thinks package installed but nothing happened, confusion when binary missing",
    legitimate_change = "Missing recipes must always fail with clear error message"
)]
#[test]
fn test_cli_install_nonexistent_package() {
    let (_dir, recipes) = create_test_env();

    let output = run_recipe(&["install", "nonexistent"], &recipes);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not found") || stderr.contains("Recipe not found"));
}

#[cheat_aware(
    protects = "Package installation completes and persists state correctly",
    severity = "HIGH",
    ease = "MEDIUM",
    cheats = ["Return success without running install()", "Skip state persistence"],
    consequence = "User thinks package is installed but it isn't, re-runs fail or behave unexpectedly",
    legitimate_change = "If install semantics change, update the lifecycle in src/core/lifecycle.rs"
)]
#[test]
fn test_cli_install_success() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "simple",
        r#"
let ctx = #{
    name: "simple",
    version: "1.0.0",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }
fn install(ctx) {
    ctx.installed = true;
    ctx
}
"#,
    );

    let output = run_recipe(&["install", "simple"], &recipes);

    assert!(
        output.status.success(),
        "Install failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify ctx was persisted
    let content = std::fs::read_to_string(recipes.join("simple.rhai")).unwrap();
    assert!(content.contains("installed: true"));
}

#[test]
fn test_cli_install_already_installed() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "already",
        r#"
let ctx = #{
    name: "already",
    version: "1.0.0",
    installed: true,
};

fn is_installed(ctx) { ctx }  // Doesn't throw = already installed

fn acquire(ctx) {
    throw "acquire should not be called";
}

fn install(ctx) {
    throw "install should not be called";
}
"#,
    );

    let output = run_recipe(&["install", "already"], &recipes);
    assert!(output.status.success());
}

#[test]
fn test_cli_accepts_explicit_rhai_path() {
    let (_dir, recipes) = create_test_env();

    // Write recipe outside the recipes directory
    let recipe_path = recipes.parent().unwrap().join("external.rhai");
    std::fs::write(
        &recipe_path,
        r#"
let ctx = #{
    name: "external",
    installed: false,
};

fn acquire(ctx) { ctx }
fn install(ctx) {
    ctx.installed = true;
    ctx
}

fn cleanup(ctx, reason) { ctx }
"#,
    )
    .unwrap();

    let output = Command::new(recipe_bin())
        .args(["install", recipe_path.to_str().unwrap()])
        .output()
        .expect("Failed to execute recipe command");

    assert!(
        output.status.success(),
        "Install failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// =============================================================================
// Remove Command Tests
// =============================================================================

#[cheat_aware(
    protects = "Package removal actually deletes installed files and updates state",
    severity = "HIGH",
    ease = "MEDIUM",
    cheats = ["Return success without calling remove()", "Update state but leave files"],
    consequence = "User thinks package is removed but files remain, disk fills up or conflicts occur",
    legitimate_change = "If remove semantics change, update the lifecycle in src/core/lifecycle.rs"
)]
#[test]
fn test_cli_remove_success() {
    let (_dir, recipes) = create_test_env();

    // Create a file that will be "removed" in the recipe's output dir
    let output_dir = recipes.join("output");
    std::fs::create_dir_all(&output_dir).unwrap();
    let test_file = output_dir.join("test-binary");
    std::fs::write(&test_file, "binary content").unwrap();

    write_recipe(
        &recipes,
        "removable",
        &format!(
            r#"
let ctx = #{{
    name: "removable",
    version: "1.0.0",
    installed: true,
    installed_file: "{}",
}};

fn remove(ctx) {{
    // Remove the file
    rm(ctx.installed_file);
    ctx.installed = false;
    ctx.installed_file = "";
    ctx
}}
"#,
            test_file.display()
        ),
    );

    assert!(test_file.exists());

    let output = run_recipe(&["remove", "removable"], &recipes);

    assert!(
        output.status.success(),
        "Remove failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // File should be deleted
    assert!(!test_file.exists());

    // State should be updated
    let content = std::fs::read_to_string(recipes.join("removable.rhai")).unwrap();
    assert!(content.contains("installed: false"));
}

#[test]
fn test_cli_remove_no_function() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "noremove",
        r#"
let ctx = #{
    name: "noremove",
    installed: true,
};
"#,
    );

    let output = run_recipe(&["remove", "noremove"], &recipes);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no remove function"));
}

// =============================================================================
// Cleanup Command Tests
// =============================================================================

#[test]
fn test_cli_cleanup_success() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "cleanable",
        r#"
let ctx = #{
    name: "cleanable",
    cache_path: "/tmp/cache",
    cleanup_reason: "",
};

fn cleanup(ctx, reason) {
    ctx.cache_path = "";
    ctx.cleanup_reason = reason;
    ctx
}
"#,
    );

    let output = run_recipe(&["cleanup", "cleanable"], &recipes);

    assert!(
        output.status.success(),
        "Cleanup failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let content = std::fs::read_to_string(recipes.join("cleanable.rhai")).unwrap();
    assert!(content.contains("cache_path: \"\""));
    assert!(content.contains("cleanup_reason: \"manual\""));
}

// =============================================================================
// List Command Tests
// =============================================================================

#[test]
fn test_cli_list_empty() {
    let (_dir, recipes) = create_test_env();

    let output = run_recipe(&["list"], &recipes);
    assert!(output.status.success());
}

#[test]
fn test_cli_list_recipes() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "pkg1",
        r#"
let ctx = #{
    name: "pkg1",
    version: "1.0.0",
};
"#,
    );

    write_recipe(
        &recipes,
        "pkg2",
        r#"
let ctx = #{
    name: "pkg2",
    version: "2.0.0",
};
"#,
    );

    let output = run_recipe(&["list"], &recipes);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("pkg1") || stdout.contains("pkg2"));
}

// =============================================================================
// Info Command Tests
// =============================================================================

#[test]
fn test_cli_info_shows_details() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "mypackage",
        r#"
let ctx = #{
    name: "mypackage",
    version: "1.2.3",
    description: "A test package",
};
"#,
    );

    let output = run_recipe(&["info", "mypackage"], &recipes);

    assert!(
        output.status.success(),
        "Info failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("mypackage"));
    assert!(stdout.contains("1.2.3"));
}

// =============================================================================
// Error Handling Tests
// =============================================================================

#[test]
fn test_cli_install_acquire_failure() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "fail-acquire",
        r#"
let ctx = #{
    name: "fail-acquire",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) {
    throw "Download failed!";
}

fn install(ctx) { ctx }
"#,
    );

    let output = run_recipe(&["install", "fail-acquire"], &recipes);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("acquire") || stderr.contains("Download failed"));
}

#[test]
fn test_cli_install_build_failure() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "fail-build",
        r#"
let ctx = #{
    name: "fail-build",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }

fn build(ctx) {
    throw "Compilation failed!";
}

fn install(ctx) { ctx }
"#,
    );

    let output = run_recipe(&["install", "fail-build"], &recipes);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("build") || stderr.contains("Compilation failed"));
}

#[test]
fn test_cli_install_install_failure() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "fail-install",
        r#"
let ctx = #{
    name: "fail-install",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }

fn install(ctx) {
    throw "Install failed!";
}
"#,
    );

    let output = run_recipe(&["install", "fail-install"], &recipes);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("install") || stderr.contains("Install failed"));
}
