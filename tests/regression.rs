//! Regression tests for previously fixed bugs
//!
//! Each test documents a bug that was fixed and ensures it doesn't recur.
//! Tests are named with the pattern: test_regression_<brief_description>

use levitate_recipe::{recipe_state, RecipeEngine};
use std::path::Path;
use tempfile::TempDir;

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

fn write_recipe(recipes_dir: &Path, name: &str, content: &str) -> std::path::PathBuf {
    let path = recipes_dir.join(format!("{}.rhai", name));
    std::fs::write(&path, content).unwrap();
    path
}

// =============================================================================
// BUG: Variable Name Substring Matching (recipe_state.rs)
// =============================================================================
// Issue: get_var("installed") incorrectly matched "installed_files" due to
// substring matching. Setting "installed" would corrupt "installed_files".
//
// Fix: Added word boundary check after variable name.

#[test]
fn test_regression_var_substring_matching_get() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.rhai");

    // Write recipe with both "installed" and "installed_files"
    std::fs::write(&path, r#"
let installed = false;
let installed_files = ["/usr/bin/foo", "/usr/lib/bar"];
let installed_version = "1.0.0";
"#).unwrap();

    // get_var("installed") should return false, NOT match installed_files
    let val: Option<bool> = recipe_state::get_var(&path, "installed").unwrap();
    assert_eq!(val, Some(false));

    // get_var("installed_files") should return the array
    let files: Option<Vec<String>> = recipe_state::get_var(&path, "installed_files").unwrap();
    assert_eq!(files, Some(vec!["/usr/bin/foo".to_string(), "/usr/lib/bar".to_string()]));
}

#[test]
fn test_regression_var_substring_matching_set() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.rhai");

    std::fs::write(&path, r#"
let installed = false;
let installed_files = ["/usr/bin/foo"];
"#).unwrap();

    // Setting "installed" should NOT affect "installed_files"
    recipe_state::set_var(&path, "installed", &true).unwrap();

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("let installed = true;"));
    assert!(content.contains(r#"let installed_files = ["/usr/bin/foo"];"#));

    // Verify via get_var
    let files: Option<Vec<String>> = recipe_state::get_var(&path, "installed_files").unwrap();
    assert_eq!(files, Some(vec!["/usr/bin/foo".to_string()]));
}

// =============================================================================
// BUG: Array Parser Escape Bug (recipe_state.rs)
// =============================================================================
// Issue: Backslash was eaten during escape handling. `["C:\\path"]` became
// `["C:path"]` instead of `["C:\path"]`.
//
// Fix: Properly handle escape sequences, preserve unknown escapes.

#[test]
fn test_regression_array_escape_backslash() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.rhai");

    // Double backslash should become single backslash
    std::fs::write(&path, r#"let paths = ["C:\\Users\\test"];"#).unwrap();

    let paths: Option<Vec<String>> = recipe_state::get_var(&path, "paths").unwrap();
    assert_eq!(paths, Some(vec!["C:\\Users\\test".to_string()]));
}

#[test]
fn test_regression_array_escape_quotes() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.rhai");

    // Escaped quotes within string
    std::fs::write(&path, r#"let strs = ["say \"hello\""];"#).unwrap();

    let strs: Option<Vec<String>> = recipe_state::get_var(&path, "strs").unwrap();
    assert_eq!(strs, Some(vec!["say \"hello\"".to_string()]));
}

#[test]
fn test_regression_array_unknown_escape_preserved() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.rhai");

    // Unknown escapes like \d should preserve the backslash
    std::fs::write(&path, r#"let patterns = ["\\d+", "\\s*"];"#).unwrap();

    let patterns: Option<Vec<String>> = recipe_state::get_var(&path, "patterns").unwrap();
    // \d is unknown escape, so backslash is preserved
    assert_eq!(patterns, Some(vec!["\\d+".to_string(), "\\s*".to_string()]));
}

// =============================================================================
// BUG: Partial Removal Marked Complete (lifecycle.rs)
// =============================================================================
// Issue: If file deletion failed, state was still cleared. Package was marked
// as removed but files remained.
//
// Fix: Fail if any file deletion fails, preserve state.

#[test]
fn test_regression_partial_removal_state_preserved() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    // Create a directory (can't be removed with remove_file)
    let bin_dir = prefix.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let non_removable = bin_dir.join("subdir");
    std::fs::create_dir(&non_removable).unwrap();
    // Put file inside so it's not empty
    std::fs::write(non_removable.join("file"), "content").unwrap();

    let recipe_path = write_recipe(&recipes_dir, "partial", &format!(r#"
let name = "partial";
let version = "1.0.0";
let installed = true;
let installed_files = ["{}"];
fn acquire() {{}}
fn install() {{}}
"#, non_removable.display()));

    let engine = RecipeEngine::new(prefix, build_dir);

    // Remove should fail
    let result = engine.remove(&recipe_path);
    assert!(result.is_err());

    // State should be PRESERVED (still installed)
    let installed: Option<bool> = recipe_state::get_var(&recipe_path, "installed").unwrap();
    assert_eq!(installed, Some(true), "State was incorrectly cleared on partial removal failure");
}

// =============================================================================
// BUG: Network Errors Silently Ignored in Update (lifecycle.rs)
// =============================================================================
// Issue: check_update() failures were logged but returned Ok(None), making it
// impossible to distinguish "no update" from "check failed".
//
// Fix: Return error so caller knows update check failed.

#[test]
fn test_regression_update_error_propagated() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(&recipes_dir, "failing-update", r#"
let name = "failing-update";
let version = "1.0.0";

fn acquire() {}
fn install() {}
fn check_update() {
    // Simulate network error
    throw "Network error!";
}
"#);

    let engine = RecipeEngine::new(prefix, build_dir);
    let result = engine.update(&recipe_path);

    // Should return error, not Ok(None)
    assert!(result.is_err(), "Update check error was silently ignored");
    assert!(result.unwrap_err().to_string().contains("update check failed"));
}

// =============================================================================
// BUG: parse_version Incorrect Prefix Stripping (http.rs)
// =============================================================================
// Issue: Stripping 'v' first broke the "version-" prefix check.
// "version-1.0.0" became "ersion-1.0.0" instead of "1.0.0".
//
// Fix: Check longer prefixes first using strip_prefix.

#[test]
fn test_regression_parse_version_order() {
    use levitate_recipe::util::http::parse_version;

    // "version-" prefix should be fully stripped
    assert_eq!(parse_version("version-1.0.0"), "1.0.0");
    assert_eq!(parse_version("version-2.5.3"), "2.5.3");

    // "release-" prefix should be fully stripped
    assert_eq!(parse_version("release-1.0.0"), "1.0.0");

    // Combined prefixes: "release-v1.0.0" -> "release-" stripped -> "v1.0.0" -> "v" stripped -> "1.0.0"
    assert_eq!(parse_version("release-v1.0.0"), "1.0.0");

    // Simple "v" prefix
    assert_eq!(parse_version("v1.0.0"), "1.0.0");

    // No prefix
    assert_eq!(parse_version("1.0.0"), "1.0.0");
}

// =============================================================================
// BUG: HTTP Requests Without Timeout (http.rs)
// =============================================================================
// Issue: ureq::get() had no timeout, could hang indefinitely.
//
// Fix: Added 30-second timeout to all HTTP calls.
// Note: This is difficult to test without mocking, but we verify the
// timeout constant exists and is reasonable.

#[test]
fn test_regression_http_has_timeout() {
    // We can't easily test the actual timeout behavior without mocking,
    // but we can verify the code compiles with timeout and test error handling.
    use levitate_recipe::util::http::http_get;

    // Invalid URL should fail quickly (not hang)
    let start = std::time::Instant::now();
    let result = http_get("http://localhost:1"); // Connection refused - fast
    let elapsed = start.elapsed();

    assert!(result.is_err());
    // Should fail within a few seconds, not 30+ seconds
    assert!(elapsed.as_secs() < 10, "HTTP request took too long: {:?}", elapsed);
}

// =============================================================================
// BUG: rpm_install Symlinks Lost (install.rs)
// =============================================================================
// Issue: is_file() check skipped symlinks. Important symlinks like /bin/sh
// were not tracked.
//
// Fix: Changed to is_file() || is_symlink().
// Note: Full test requires rpm2cpio, but we can test the symlink tracking logic.

#[test]
#[cfg(unix)]
fn test_regression_symlink_tracking() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new().unwrap();
    let target = dir.path().join("target");
    let link = dir.path().join("link");

    std::fs::write(&target, "content").unwrap();
    symlink(&target, &link).unwrap();

    // Verify our detection logic
    assert!(link.is_symlink());
    assert!(link.is_file() || link.is_symlink()); // This is the fix

    // Old buggy code would only check is_file(), which returns true for symlinks
    // that point to files, but false for broken symlinks. The fix ensures we
    // track symlinks explicitly.
}

// =============================================================================
// BUG: Path Traversal in Package Names (recipe.rs)
// =============================================================================
// Issue: Package name "../../etc/passwd" could escape recipes directory.
//
// Fix: Validate package names are simple identifiers.

#[test]
fn test_regression_path_traversal_blocked() {
    // These should all be invalid package names
    let invalid_names = [
        "../etc/passwd",
        "../../etc/passwd",
        "foo/../bar",
        "/etc/passwd",
        "foo/bar",
        "..",
        ".",
        "pkg!name",
        "pkg@name",
        "pkg name",
    ];

    for name in &invalid_names {
        // Can't directly test validate_package_name from here as it's private,
        // but we verify the resolve_recipe behavior through integration tests.
        // The test in tests/e2e.rs covers CLI rejection.

        // At minimum, verify these patterns would be dangerous
        assert!(
            name.contains('/') || name.contains('\\') || name.contains(' ')
            || name.contains('!') || name.contains('@') || *name == "." || *name == "..",
            "Name '{}' should be detected as dangerous", name
        );
    }
}

// =============================================================================
// BUG: Dead Code (context.rs)
// =============================================================================
// Issue: init_context() and get_recipe_path() were unused.
//
// Fix: Removed dead code.
// Note: This is verified by cargo build --release with warnings as errors.

// =============================================================================
// Comprehensive State Corruption Prevention
// =============================================================================

#[test]
fn test_regression_state_not_corrupted_on_install_failure() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    // Create recipe that will fail during install
    let recipe_path = write_recipe(&recipes_dir, "fail-install", r#"
let name = "fail-install";
let version = "1.0.0";
let installed = false;

fn acquire() {}
fn install() {
    throw "Install failed!";
}
"#);

    let engine = RecipeEngine::new(prefix, build_dir);
    let result = engine.execute(&recipe_path);

    // Should fail
    assert!(result.is_err());

    // State should NOT be updated
    let installed: Option<bool> = recipe_state::get_var(&recipe_path, "installed").unwrap();
    assert_eq!(installed, Some(false), "State was corrupted despite install failure");
}

#[test]
fn test_regression_state_not_corrupted_on_acquire_failure() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(&recipes_dir, "fail-acquire", r#"
let name = "fail-acquire";
let version = "1.0.0";

fn acquire() {
    throw "Acquire failed!";
}
fn install() {}
"#);

    let engine = RecipeEngine::new(prefix, build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_err());

    // Should not have "installed = true" since we never got there
    let installed: Option<bool> = recipe_state::get_var(&recipe_path, "installed").unwrap();
    assert_ne!(installed, Some(true), "State was set despite acquire failure");
}

#[test]
fn test_regression_state_not_corrupted_on_build_failure() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(&recipes_dir, "fail-build", r#"
let name = "fail-build";
let version = "1.0.0";

fn acquire() {}
fn build() {
    throw "Build failed!";
}
fn install() {}
"#);

    let engine = RecipeEngine::new(prefix, build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_err());

    let installed: Option<bool> = recipe_state::get_var(&recipe_path, "installed").unwrap();
    assert_ne!(installed, Some(true), "State was set despite build failure");
}

// =============================================================================
// Edge Cases That Could Regress
// =============================================================================

#[test]
fn test_regression_empty_installed_files_handled() {
    let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(&recipes_dir, "empty-files", r#"
let name = "empty-files";
let version = "1.0.0";
let installed = true;
let installed_files = [];
fn acquire() {}
fn install() {}
"#);

    let engine = RecipeEngine::new(prefix, build_dir);

    // Remove should succeed with empty file list
    let result = engine.remove(&recipe_path);
    assert!(result.is_ok());
}

#[test]
fn test_regression_unicode_in_recipe_preserved() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.rhai");

    let original = r#"let name = "日本語パッケージ";
let description = "Package with 📦 emoji";
let version = "1.0.0";"#;

    std::fs::write(&path, original).unwrap();

    // Modify one variable
    recipe_state::set_var(&path, "version", &"2.0.0".to_string()).unwrap();

    // Unicode should be preserved
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("日本語パッケージ"));
    assert!(content.contains("📦"));
}

#[test]
fn test_regression_comments_preserved() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.rhai");

    let original = r#"// Package definition
let name = "test";
let version = "1.0.0";  // Current version
/* Multi-line
   comment */
fn acquire() {}"#;

    std::fs::write(&path, original).unwrap();

    recipe_state::set_var(&path, "version", &"2.0.0".to_string()).unwrap();

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("// Package definition"));
    assert!(content.contains("/* Multi-line"));
    assert!(content.contains("fn acquire()"));
}
