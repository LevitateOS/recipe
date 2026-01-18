//! Integration tests for recipe lifecycle
//!
//! These tests verify that multiple components work together correctly.

use levitate_recipe::{recipe_state, RecipeEngine};
use std::path::Path;
use tempfile::TempDir;

/// Create a test environment with prefix, build_dir, and recipes directories
fn create_test_env() -> (TempDir, std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let prefix = dir.path().join("prefix");
    let build_dir = dir.path().join("build");
    let recipes_dir = dir.path().join("recipes");
    std::fs::create_dir_all(&prefix).unwrap();
    std::fs::create_dir_all(&build_dir).unwrap();
    std::fs::create_dir_all(&recipes_dir).unwrap();
    (dir, prefix, build_dir, recipes_dir)
}

/// Write a recipe file and return its path
fn write_recipe(recipes_dir: &Path, name: &str, content: &str) -> std::path::PathBuf {
    let path = recipes_dir.join(format!("{}.rhai", name));
    std::fs::write(&path, content).unwrap();
    path
}

// =============================================================================
// Full Lifecycle Tests
// =============================================================================

#[test]
fn test_full_install_lifecycle() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    // Create a simple recipe that creates a file
    let recipe_path = write_recipe(&recipes_dir, "simple", &format!(r#"
let name = "simple";
let version = "1.0.0";
let description = "A simple test package";

fn acquire() {{
    // Nothing to acquire - we'll create files directly
}}

fn install() {{
    // Create a file in prefix/bin
    let bin_dir = `{}/bin`;
    run(`mkdir -p ${{bin_dir}}`);
    run(`echo '#!/bin/sh\necho hello' > ${{bin_dir}}/simple-cmd`);
    run(`chmod +x ${{bin_dir}}/simple-cmd`);
}}
"#, prefix.display()));

    let engine = RecipeEngine::new(prefix.clone(), build_dir);

    // Execute should succeed
    let result = engine.execute(&recipe_path);
    assert!(result.is_ok(), "Execute failed: {:?}", result.err());

    // Verify state was updated
    let installed: Option<bool> = recipe_state::get_var(&recipe_path, "installed").unwrap();
    assert_eq!(installed, Some(true));

    let installed_version: Option<recipe_state::OptionalString> =
        recipe_state::get_var(&recipe_path, "installed_version").unwrap();
    assert!(matches!(installed_version, Some(recipe_state::OptionalString::Some(ref v)) if v == "1.0.0"));
}

#[test]
fn test_install_then_remove_lifecycle() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    // Create a test file to track
    let test_file = prefix.join("bin/test-binary");
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    std::fs::write(&test_file, "binary content").unwrap();

    // Create recipe with pre-existing file in installed_files
    let recipe_path = write_recipe(&recipes_dir, "removable", &format!(r#"
let name = "removable";
let version = "1.0.0";
let installed = true;
let installed_version = "1.0.0";
let installed_files = ["{}"];

fn acquire() {{}}
fn install() {{}}
"#, test_file.display()));

    let engine = RecipeEngine::new(prefix.clone(), build_dir);

    // File should exist
    assert!(test_file.exists());

    // Remove should succeed
    let result = engine.remove(&recipe_path);
    assert!(result.is_ok(), "Remove failed: {:?}", result.err());

    // File should be deleted
    assert!(!test_file.exists());

    // State should be updated
    let installed: Option<bool> = recipe_state::get_var(&recipe_path, "installed").unwrap();
    assert_eq!(installed, Some(false));
}

#[test]
fn test_reinstall_after_remove() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(&recipes_dir, "reinstall", r#"
let name = "reinstall";
let version = "1.0.0";

fn acquire() {}
fn install() {}
"#);

    let engine = RecipeEngine::new(prefix.clone(), build_dir);

    // First install
    let result = engine.execute(&recipe_path);
    assert!(result.is_ok());

    let installed: Option<bool> = recipe_state::get_var(&recipe_path, "installed").unwrap();
    assert_eq!(installed, Some(true));

    // Set to not installed (simulating remove)
    recipe_state::set_var(&recipe_path, "installed", &false).unwrap();
    recipe_state::set_var(&recipe_path, "installed_files", &Vec::<String>::new()).unwrap();

    // Should be able to reinstall
    let result = engine.execute(&recipe_path);
    assert!(result.is_ok());

    let installed: Option<bool> = recipe_state::get_var(&recipe_path, "installed").unwrap();
    assert_eq!(installed, Some(true));
}

#[test]
fn test_skip_already_installed() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(&recipes_dir, "already-installed", r#"
let name = "already-installed";
let version = "1.0.0";
let installed = true;
let installed_version = "1.0.0";

fn acquire() {
    // This should not be called if already installed
    throw "acquire should not be called";
}

fn install() {
    throw "install should not be called";
}
"#);

    let engine = RecipeEngine::new(prefix.clone(), build_dir);

    // Should skip without error (not call acquire/install which would throw)
    let result = engine.execute(&recipe_path);
    assert!(result.is_ok());
}

// =============================================================================
// Update/Upgrade Lifecycle Tests
// =============================================================================

#[test]
fn test_update_finds_new_version() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(&recipes_dir, "updatable", r#"
let name = "updatable";
let version = "1.0.0";

fn acquire() {}
fn install() {}
fn check_update() {
    "2.0.0"
}
"#);

    let engine = RecipeEngine::new(prefix, build_dir);

    let result = engine.update(&recipe_path);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some("2.0.0".to_string()));

    // Version should be updated in recipe
    let version: Option<String> = recipe_state::get_var(&recipe_path, "version").unwrap();
    assert_eq!(version, Some("2.0.0".to_string()));
}

#[test]
fn test_update_no_new_version() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(&recipes_dir, "up-to-date", r#"
let name = "up-to-date";
let version = "1.0.0";

fn acquire() {}
fn install() {}
fn check_update() {
    ()  // No update available
}
"#);

    let engine = RecipeEngine::new(prefix, build_dir);

    let result = engine.update(&recipe_path);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), None);
}

#[test]
fn test_upgrade_when_version_differs() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(&recipes_dir, "upgradable", r#"
let name = "upgradable";
let version = "2.0.0";
let installed = true;
let installed_version = "1.0.0";
let installed_files = [];

fn acquire() {}
fn install() {}
"#);

    let engine = RecipeEngine::new(prefix.clone(), build_dir.clone());

    let result = engine.upgrade(&recipe_path);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), true); // Upgrade was performed

    // installed_version should now match version
    let installed_version: Option<recipe_state::OptionalString> =
        recipe_state::get_var(&recipe_path, "installed_version").unwrap();
    assert!(matches!(installed_version, Some(recipe_state::OptionalString::Some(ref v)) if v == "2.0.0"));
}

#[test]
fn test_upgrade_skipped_when_up_to_date() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(&recipes_dir, "no-upgrade", r#"
let name = "no-upgrade";
let version = "1.0.0";
let installed = true;
let installed_version = "1.0.0";
let installed_files = [];

fn acquire() {}
fn install() {}
"#);

    let engine = RecipeEngine::new(prefix, build_dir);

    let result = engine.upgrade(&recipe_path);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), false); // No upgrade needed
}

// =============================================================================
// State Persistence Tests
// =============================================================================

#[test]
fn test_state_persists_after_install() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(&recipes_dir, "persistent", r#"
let name = "persistent";
let version = "1.0.0";

fn acquire() {}
fn install() {}
"#);

    let engine = RecipeEngine::new(prefix, build_dir);
    engine.execute(&recipe_path).unwrap();

    // Read the file content directly
    let content = std::fs::read_to_string(&recipe_path).unwrap();

    // Verify state variables are in the file
    assert!(content.contains("let installed = true;"));
    assert!(content.contains("let installed_version = \"1.0.0\";"));
    assert!(content.contains("let installed_at = "));
    assert!(content.contains("let installed_files = "));
}

#[test]
fn test_state_preserves_original_content() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let original = r#"// Comment at top
let name = "preserve-content";
let version = "1.0.0";
let description = "Test description";

fn acquire() {
    // acquire logic
}

fn install() {
    // install logic
}
"#;

    let recipe_path = write_recipe(&recipes_dir, "preserve-content", original);
    let engine = RecipeEngine::new(prefix, build_dir);
    engine.execute(&recipe_path).unwrap();

    let content = std::fs::read_to_string(&recipe_path).unwrap();

    // Original content should be preserved
    assert!(content.contains("// Comment at top"));
    assert!(content.contains("let description = \"Test description\";"));
    assert!(content.contains("fn acquire()"));
    assert!(content.contains("fn install()"));
}

// =============================================================================
// Error Handling Tests
// =============================================================================

#[test]
fn test_execute_missing_acquire() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(&recipes_dir, "no-acquire", r#"
let name = "no-acquire";
let version = "1.0.0";

fn install() {}
"#);

    let engine = RecipeEngine::new(prefix, build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("acquire"));
}

#[test]
fn test_execute_missing_install() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(&recipes_dir, "no-install", r#"
let name = "no-install";
let version = "1.0.0";

fn acquire() {}
"#);

    let engine = RecipeEngine::new(prefix, build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("install"));
}

#[test]
fn test_execute_acquire_failure() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(&recipes_dir, "acquire-fail", r#"
let name = "acquire-fail";
let version = "1.0.0";

fn acquire() {
    throw "Acquire failed!";
}

fn install() {}
"#);

    let engine = RecipeEngine::new(prefix, build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("acquire"));

    // State should NOT be updated on failure
    let installed: Option<bool> = recipe_state::get_var(&recipe_path, "installed").unwrap_or(None);
    assert_ne!(installed, Some(true));
}

#[test]
fn test_execute_install_failure() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(&recipes_dir, "install-fail", r#"
let name = "install-fail";
let version = "1.0.0";

fn acquire() {}

fn install() {
    throw "Install failed!";
}
"#);

    let engine = RecipeEngine::new(prefix, build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("install"));

    // State should NOT be updated on failure
    let installed: Option<bool> = recipe_state::get_var(&recipe_path, "installed").unwrap_or(None);
    assert_ne!(installed, Some(true));
}

// =============================================================================
// Build Phase Tests
// =============================================================================

#[test]
fn test_optional_build_phase() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    // Recipe without build phase
    let recipe_path = write_recipe(&recipes_dir, "no-build", r#"
let name = "no-build";
let version = "1.0.0";

fn acquire() {}
fn install() {}
"#);

    let engine = RecipeEngine::new(prefix, build_dir);
    let result = engine.execute(&recipe_path);

    // Should succeed without build phase
    assert!(result.is_ok());
}

#[test]
fn test_with_build_phase() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(&recipes_dir, "with-build", r#"
let name = "with-build";
let version = "1.0.0";

fn acquire() {}
fn build() {
    // Build phase runs
}
fn install() {}
"#);

    let engine = RecipeEngine::new(prefix, build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_ok());
}

#[test]
fn test_build_failure_prevents_install() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(&recipes_dir, "build-fail", r#"
let name = "build-fail";
let version = "1.0.0";

fn acquire() {}
fn build() {
    throw "Build failed!";
}
fn install() {
    throw "Install should not be reached!";
}
"#);

    let engine = RecipeEngine::new(prefix, build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("build"));
}

// =============================================================================
// is_installed() Function Tests
// =============================================================================

#[test]
fn test_is_installed_function_overrides_state() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    // Recipe says installed=true but is_installed() returns false
    let recipe_path = write_recipe(&recipes_dir, "check-installed", r#"
let name = "check-installed";
let version = "1.0.0";
let installed = true;

fn is_installed() {
    false  // Files were deleted
}

fn acquire() {}
fn install() {}
"#);

    let engine = RecipeEngine::new(prefix, build_dir);

    // Should proceed with install since is_installed() returns false
    let result = engine.execute(&recipe_path);
    assert!(result.is_ok());
}

// =============================================================================
// Concurrent Operations Tests
// =============================================================================

#[test]
fn test_multiple_recipes_independent() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe1 = write_recipe(&recipes_dir, "pkg1", r#"
let name = "pkg1";
let version = "1.0.0";
fn acquire() {}
fn install() {}
"#);

    let recipe2 = write_recipe(&recipes_dir, "pkg2", r#"
let name = "pkg2";
let version = "2.0.0";
fn acquire() {}
fn install() {}
"#);

    let engine = RecipeEngine::new(prefix, build_dir);

    // Install both
    engine.execute(&recipe1).unwrap();
    engine.execute(&recipe2).unwrap();

    // Both should be installed independently
    let pkg1_installed: Option<bool> = recipe_state::get_var(&recipe1, "installed").unwrap();
    let pkg2_installed: Option<bool> = recipe_state::get_var(&recipe2, "installed").unwrap();

    assert_eq!(pkg1_installed, Some(true));
    assert_eq!(pkg2_installed, Some(true));

    // Versions should be correct
    let pkg1_ver: Option<recipe_state::OptionalString> =
        recipe_state::get_var(&recipe1, "installed_version").unwrap();
    let pkg2_ver: Option<recipe_state::OptionalString> =
        recipe_state::get_var(&recipe2, "installed_version").unwrap();

    assert!(matches!(pkg1_ver, Some(recipe_state::OptionalString::Some(ref v)) if v == "1.0.0"));
    assert!(matches!(pkg2_ver, Some(recipe_state::OptionalString::Some(ref v)) if v == "2.0.0"));
}
