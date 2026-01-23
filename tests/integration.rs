//! Integration tests for recipe lifecycle
//!
//! These tests verify that multiple components work together correctly.

use leviso_cheat_test::{cheat_aware, cheat_canary};
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

#[cheat_aware(
    protects = "User can install a package and have it tracked correctly",
    severity = "CRITICAL",
    ease = "MEDIUM",
    cheats = [
        "Return Ok without actually running acquire/install functions",
        "Check only for execute success, not state persistence",
        "Use recipe that doesn't actually install anything"
    ],
    consequence = "User installs package, state says installed but nothing actually happened"
)]
#[test]
fn test_full_install_lifecycle() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    // Create a simple recipe that creates a file
    let recipe_path = write_recipe(&recipes_dir, "simple", &format!(r#"
let name = "simple";
let version = "1.0.0";
let installed = false;
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

#[cheat_aware(
    protects = "User can remove installed packages and files are actually deleted",
    severity = "CRITICAL",
    ease = "MEDIUM",
    cheats = [
        "Mark installed=false without actually deleting files",
        "Skip installed_files tracking to avoid deletion",
        "Check only for state update, not file deletion"
    ],
    consequence = "User removes package, state says removed but files still consuming disk space"
)]
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

#[cheat_aware(
    protects = "User can reinstall a previously removed package",
    severity = "MEDIUM",
    ease = "EASY",
    cheats = [
        "Keep installed=true after simulated remove",
        "Skip testing actual reinstall functionality",
        "Test only fresh install, not reinstall path"
    ],
    consequence = "User tries to reinstall removed package, gets 'already installed' error"
)]
#[test]
fn test_reinstall_after_remove() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(&recipes_dir, "reinstall", r#"
let name = "reinstall";
let version = "1.0.0";
let installed = false;

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

#[cheat_aware(
    protects = "Already-installed packages are skipped efficiently",
    severity = "MEDIUM",
    ease = "EASY",
    cheats = [
        "Run acquire/install anyway but still return success",
        "Check only for success, not that functions weren't called",
        "Use installed=false in test to avoid testing this path"
    ],
    consequence = "User runs install on existing package, wastes time re-downloading and reinstalling"
)]
#[test]
fn test_skip_already_installed() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(&recipes_dir, "already-installed", r#"
let name = "already-installed";
let version = "1.0.0";
let installed = true;
let installed_version = "1.0.0";
let installed_files = [];

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

#[cheat_aware(
    protects = "User can check for new versions of packages",
    severity = "HIGH",
    ease = "MEDIUM",
    cheats = [
        "Return hardcoded version without calling check_update",
        "Check only that update returns something, not correct version",
        "Skip verification that version field was updated"
    ],
    consequence = "User checks for updates, gets wrong version or misses critical update"
)]
#[test]
fn test_update_finds_new_version() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(&recipes_dir, "updatable", r#"
let name = "updatable";
let version = "1.0.0";
let installed = false;

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

#[cheat_aware(
    protects = "User gets accurate 'no update' when package is current",
    severity = "MEDIUM",
    ease = "EASY",
    cheats = [
        "Always return Some version from update",
        "Skip checking return value for None case",
        "Use check_update that always returns version"
    ],
    consequence = "User thinks update is available when package is already current"
)]
#[test]
fn test_update_no_new_version() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(&recipes_dir, "up-to-date", r#"
let name = "up-to-date";
let version = "1.0.0";
let installed = false;

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

#[cheat_aware(
    protects = "User can upgrade packages to new versions",
    severity = "CRITICAL",
    ease = "MEDIUM",
    cheats = [
        "Mark version as upgraded without running install",
        "Check only for return value, not actual upgrade",
        "Use same version in test to avoid upgrade path"
    ],
    consequence = "User runs upgrade, version bumped in state but old binary still installed"
)]
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

#[cheat_aware(
    protects = "User doesn't waste time upgrading already-current packages",
    severity = "LOW",
    ease = "EASY",
    cheats = [
        "Always run upgrade regardless of version match",
        "Return false but still run upgrade",
        "Use different versions in test to avoid this path"
    ],
    consequence = "User runs upgrade on current package, wastes time reinstalling same version"
)]
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

#[cheat_aware(
    protects = "Installation state persists correctly to recipe file",
    severity = "CRITICAL",
    ease = "MEDIUM",
    cheats = [
        "Keep state in memory only, don't write to file",
        "Check in-memory state, not file contents",
        "Read state from same engine instance"
    ],
    consequence = "User installs package, restarts, state is lost - installed=false again"
)]
#[test]
fn test_state_persists_after_install() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(&recipes_dir, "persistent", r#"
let name = "persistent";
let version = "1.0.0";
let installed = false;

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

#[cheat_aware(
    protects = "Original recipe content is preserved during state updates",
    severity = "HIGH",
    ease = "MEDIUM",
    cheats = [
        "Overwrite entire file with minimal state",
        "Check only for state variables, not original content",
        "Use recipe with no special content to preserve"
    ],
    consequence = "User installs package, comments/descriptions/custom fields are lost"
)]
#[test]
fn test_state_preserves_original_content() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let original = r#"// Comment at top
let name = "preserve-content";
let version = "1.0.0";
let installed = false;
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

#[cheat_aware(
    protects = "Missing acquire function is detected and reported",
    severity = "HIGH",
    ease = "EASY",
    cheats = [
        "Skip function validation entirely",
        "Accept empty acquire as valid",
        "Check only for any error, not specific message"
    ],
    consequence = "Recipe without acquire passes validation, fails mysteriously during install"
)]
#[test]
fn test_execute_missing_acquire() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(&recipes_dir, "no-acquire", r#"
let name = "no-acquire";
let version = "1.0.0";
let installed = false;

fn install() {}
"#);

    let engine = RecipeEngine::new(prefix, build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("acquire"));
}

#[cheat_aware(
    protects = "Missing install function is detected and reported",
    severity = "HIGH",
    ease = "EASY",
    cheats = [
        "Skip function validation entirely",
        "Accept empty install as valid",
        "Check only for any error, not specific message"
    ],
    consequence = "Recipe without install passes validation, nothing gets installed"
)]
#[test]
fn test_execute_missing_install() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(&recipes_dir, "no-install", r#"
let name = "no-install";
let version = "1.0.0";
let installed = false;

fn acquire() {}
"#);

    let engine = RecipeEngine::new(prefix, build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("install"));
}

#[cheat_aware(
    protects = "Acquire failures are caught and state is not corrupted",
    severity = "CRITICAL",
    ease = "MEDIUM",
    cheats = [
        "Catch error but still update state to installed=true",
        "Check only that error is returned, not state",
        "Use acquire that succeeds to avoid error path"
    ],
    consequence = "Acquire fails but installed=true, user thinks package works but binaries missing"
)]
#[test]
fn test_execute_acquire_failure() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(&recipes_dir, "acquire-fail", r#"
let name = "acquire-fail";
let version = "1.0.0";
let installed = false;

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

#[cheat_aware(
    protects = "Install failures are caught and state is not corrupted",
    severity = "CRITICAL",
    ease = "MEDIUM",
    cheats = [
        "Catch error but still update state to installed=true",
        "Check only that error is returned, not state",
        "Use install that succeeds to avoid error path"
    ],
    consequence = "Install fails but installed=true, user thinks package works but it's broken"
)]
#[test]
fn test_execute_install_failure() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(&recipes_dir, "install-fail", r#"
let name = "install-fail";
let version = "1.0.0";
let installed = false;

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

#[cheat_aware(
    protects = "Recipes without build phase still work correctly",
    severity = "MEDIUM",
    ease = "EASY",
    cheats = [
        "Require build phase for all recipes",
        "Use recipe with build phase in test",
        "Skip testing optional-build path"
    ],
    consequence = "Simple recipes without build phase fail to install"
)]
#[test]
fn test_optional_build_phase() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    // Recipe without build phase
    let recipe_path = write_recipe(&recipes_dir, "no-build", r#"
let name = "no-build";
let version = "1.0.0";
let installed = false;

fn acquire() {}
fn install() {}
"#);

    let engine = RecipeEngine::new(prefix, build_dir);
    let result = engine.execute(&recipe_path);

    // Should succeed without build phase
    assert!(result.is_ok());
}

#[cheat_aware(
    protects = "Build phase is executed when present",
    severity = "MEDIUM",
    ease = "EASY",
    cheats = [
        "Skip build phase entirely",
        "Check only for success, not that build ran",
        "Use empty build that does nothing"
    ],
    consequence = "User's build phase is skipped, package installs uncompiled source"
)]
#[test]
fn test_with_build_phase() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(&recipes_dir, "with-build", r#"
let name = "with-build";
let version = "1.0.0";
let installed = false;

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

#[cheat_aware(
    protects = "Build failures prevent install from running",
    severity = "HIGH",
    ease = "MEDIUM",
    cheats = [
        "Continue to install after build fails",
        "Check only for error, not that install was skipped",
        "Use build that succeeds"
    ],
    consequence = "Build fails but install runs anyway, user gets broken/uncompiled package"
)]
#[test]
fn test_build_failure_prevents_install() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(&recipes_dir, "build-fail", r#"
let name = "build-fail";
let version = "1.0.0";
let installed = false;

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

#[cheat_aware(
    protects = "is_installed() function can override state for verification",
    severity = "HIGH",
    ease = "MEDIUM",
    cheats = [
        "Ignore is_installed() return value",
        "Check only installed variable, not function",
        "Test without is_installed() function"
    ],
    consequence = "Package marked installed but binaries deleted, is_installed() ignored, user confused"
)]
#[test]
fn test_is_installed_function_overrides_state() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    // Recipe says installed=true but is_installed() returns false
    let recipe_path = write_recipe(&recipes_dir, "check-installed", r#"
let name = "check-installed";
let version = "1.0.0";
let installed = true;
let installed_version = "1.0.0";
let installed_files = [];

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

#[cheat_aware(
    protects = "Multiple packages can be installed independently",
    severity = "MEDIUM",
    ease = "EASY",
    cheats = [
        "Use same state for all packages",
        "Test only single package",
        "Skip verification of independent state"
    ],
    consequence = "Installing pkg2 corrupts pkg1 state, or vice versa"
)]
#[test]
fn test_multiple_recipes_independent() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe1 = write_recipe(&recipes_dir, "pkg1", r#"
let name = "pkg1";
let version = "1.0.0";
let installed = false;
fn acquire() {}
fn install() {}
"#);

    let recipe2 = write_recipe(&recipes_dir, "pkg2", r#"
let name = "pkg2";
let version = "2.0.0";
let installed = false;
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

// =============================================================================
// Canary test - DO NOT MODIFY without triggering review
// =============================================================================

/// CANARY TEST: Verbose recipe state verification.
///
/// This test checks each recipe state variable individually. A cheater would
/// want to consolidate these checks. Any modification triggers review.
#[cheat_canary(
    bait = "Individual state checks look tedious - tempting to simplify",
    tripwire = "Any modification to this function triggers full audit of recipe test changes"
)]
#[test]
fn canary_recipe_verbose_state_check() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe = write_recipe(&recipes_dir, "canary_pkg", r#"
let name = "canary_pkg";
let version = "9.9.9";
let installed = false;
fn acquire() {}
fn install() {}
"#);

    let engine = RecipeEngine::new(prefix, build_dir);
    engine.execute(&recipe).unwrap();

    // DO NOT consolidate these into a single check or helper function.
    // Each assertion must be individual to make cheating obvious.

    // Check 1: installed flag
    let installed: Option<bool> = recipe_state::get_var(&recipe, "installed").unwrap();
    assert_eq!(installed, Some(true), "installed flag not set to true");

    // Check 2: name unchanged
    let name: Option<String> = recipe_state::get_var(&recipe, "name").unwrap();
    assert_eq!(name.as_deref(), Some("canary_pkg"), "name was modified unexpectedly");

    // Check 3: version unchanged
    let version: Option<String> = recipe_state::get_var(&recipe, "version").unwrap();
    assert_eq!(version.as_deref(), Some("9.9.9"), "version was modified unexpectedly");

    // Check 4: installed_version matches
    let installed_version: Option<recipe_state::OptionalString> =
        recipe_state::get_var(&recipe, "installed_version").unwrap();
    assert!(
        matches!(installed_version, Some(recipe_state::OptionalString::Some(ref v)) if v == "9.9.9"),
        "installed_version does not match version: {:?}",
        installed_version
    );

    // Check 5: recipe file still exists
    assert!(recipe.exists(), "recipe file was deleted");

    // Check 6: recipe file is readable
    let content = std::fs::read_to_string(&recipe).unwrap();
    assert!(content.contains("canary_pkg"), "recipe content corrupted");

    // Check 7: recipe contains installed = true after execution
    assert!(content.contains("installed = true"), "installed flag not persisted to file");
}
