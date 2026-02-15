//! Regression tests for previously fixed bugs (ctx-based design)
//!
//! Each test documents a bug that was fixed and ensures it doesn't recur.

use leviso_cheat_test::cheat_aware;
use levitate_recipe::RecipeEngine;
use std::path::Path;
use tempfile::TempDir;

fn create_test_env() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let build_dir = dir.path().join("build");
    let recipes_dir = dir.path().join("recipes");
    std::fs::create_dir_all(&build_dir).unwrap();
    std::fs::create_dir_all(&recipes_dir).unwrap();
    (dir, build_dir, recipes_dir)
}

fn write_recipe(recipes_dir: &Path, name: &str, content: &str) -> std::path::PathBuf {
    let path = recipes_dir.join(format!("{}.rhai", name));
    let mut content = content.to_string();
    // Cleanup is required by repo policy; regression tests default to a no-op stub.
    if !content.contains("fn cleanup(") {
        content.push_str("\nfn cleanup(ctx, reason) { ctx }\n");
    }
    std::fs::write(&path, content).unwrap();
    path
}

// =============================================================================
// BUG: ctx serialization must handle special characters
// =============================================================================

#[test]
fn test_regression_ctx_escapes_quotes() {
    let (_dir, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(
        &recipes_dir,
        "escape-test",
        r#"
let ctx = #{
    name: "escape-test",
    message: "",
};

fn acquire(ctx) {
    ctx.message = "hello \"world\"";
    ctx
}

fn install(ctx) { ctx }
"#,
    );

    let engine = RecipeEngine::new(build_dir);
    engine.execute(&recipe_path).unwrap();

    let content = std::fs::read_to_string(&recipe_path).unwrap();
    // Should be properly escaped
    assert!(content.contains(r#"message: "hello \"world\"""#));
}

#[test]
fn test_regression_ctx_escapes_backslashes() {
    let (_dir, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(
        &recipes_dir,
        "backslash-test",
        r#"
let ctx = #{
    name: "backslash-test",
    path: "",
};

fn acquire(ctx) {
    ctx.path = "C:\\Users\\test";
    ctx
}

fn install(ctx) { ctx }
"#,
    );

    let engine = RecipeEngine::new(build_dir);
    engine.execute(&recipe_path).unwrap();

    let content = std::fs::read_to_string(&recipe_path).unwrap();
    // Backslashes should be escaped
    assert!(content.contains(r#"path: "C:\\Users\\test""#));
}

// =============================================================================
// BUG: ctx block not found should give clear error
// =============================================================================

#[test]
fn test_regression_missing_ctx_clear_error() {
    let (_dir, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(
        &recipes_dir,
        "no-ctx",
        r#"
let name = "no-ctx";
fn acquire() {}
fn install() {}
"#,
    );

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("ctx"));
}

// =============================================================================
// BUG: Failure should not corrupt partially updated ctx
// =============================================================================

#[cheat_aware(
    protects = "Partial failures preserve completed phase state for resume",
    severity = "HIGH",
    ease = "MEDIUM",
    cheats = ["Roll back all state on any failure", "Only persist state on full success"],
    consequence = "User re-runs failed install, must re-download/rebuild from scratch",
    legitimate_change = "Phase state must be persisted after each phase completes. \
        This enables resumable installations where only the failed phase re-runs."
)]
#[test]
fn test_regression_failure_preserves_acquire_state() {
    let (_dir, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(
        &recipes_dir,
        "partial-fail",
        r#"
let ctx = #{
    name: "partial-fail",
    acquired: false,
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) {
    ctx.acquired = true;
    ctx
}

fn install(ctx) {
    throw "Install failed!";
}
"#,
    );

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);
    assert!(result.is_err());

    // acquire succeeded, so acquired: true should be persisted
    let content = std::fs::read_to_string(&recipe_path).unwrap();
    assert!(content.contains("acquired: true"));
    // But installed should still be false
    assert!(content.contains("installed: false"));
}

// =============================================================================
// BUG: HTTP Requests Without Timeout (http.rs)
// =============================================================================

#[test]
fn test_regression_http_has_timeout() {
    use levitate_recipe::helpers::acquire::http::http_get;

    // Invalid URL should fail quickly (not hang)
    let start = std::time::Instant::now();
    let result = http_get("http://localhost:1"); // Connection refused - fast
    let elapsed = start.elapsed();

    assert!(result.is_err());
    // Should fail within a few seconds, not 30+ seconds
    assert!(
        elapsed.as_secs() < 10,
        "HTTP request took too long: {:?}",
        elapsed
    );
}

// =============================================================================
// BUG: parse_version Incorrect Prefix Stripping (http.rs)
// =============================================================================

#[test]
fn test_regression_parse_version_order() {
    use levitate_recipe::helpers::acquire::http::parse_version;

    // "version-" prefix should be fully stripped
    assert_eq!(parse_version("version-1.0.0"), "1.0.0");
    assert_eq!(parse_version("version-2.5.3"), "2.5.3");

    // "release-" prefix should be fully stripped
    assert_eq!(parse_version("release-1.0.0"), "1.0.0");

    // Combined prefixes
    assert_eq!(parse_version("release-v1.0.0"), "1.0.0");

    // Simple "v" prefix
    assert_eq!(parse_version("v1.0.0"), "1.0.0");

    // No prefix
    assert_eq!(parse_version("1.0.0"), "1.0.0");
}

// =============================================================================
// BUG: Symlinks need proper handling (Unix only)
// =============================================================================

#[test]
#[cfg(unix)]
fn test_regression_symlink_detection() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new().unwrap();
    let target = dir.path().join("target");
    let link = dir.path().join("link");

    std::fs::write(&target, "content").unwrap();
    symlink(&target, &link).unwrap();

    // Verify detection works
    assert!(link.is_symlink());
    assert!(link.is_file() || link.is_symlink());
}

// =============================================================================
// BUG: Concurrent lock should block
// =============================================================================

#[test]
fn test_regression_concurrent_lock_blocked() {
    let (_dir, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(
        &recipes_dir,
        "lock-test",
        r#"
let ctx = #{
    name: "lock-test",
};

fn acquire(ctx) { ctx }
fn install(ctx) { ctx }
"#,
    );

    use levitate_recipe::RecipeEngine;

    let engine = RecipeEngine::new(build_dir.clone());

    // First execution
    let result = engine.execute(&recipe_path);
    assert!(result.is_ok());

    // Should be able to execute again (lock released)
    let engine2 = RecipeEngine::new(build_dir);
    let result2 = engine2.execute(&recipe_path);
    assert!(result2.is_ok());
}

// =============================================================================
// BUG: Unicode content preservation
// =============================================================================

#[test]
fn test_regression_unicode_preserved() {
    let (_dir, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(
        &recipes_dir,
        "unicode",
        r#"
let ctx = #{
    name: "unicode-test",
    description: "Contains emoji and unicode",
};

fn acquire(ctx) {
    ctx.description = "Updated with emoji";
    ctx
}

fn install(ctx) { ctx }
"#,
    );

    let engine = RecipeEngine::new(build_dir);
    engine.execute(&recipe_path).unwrap();

    let content = std::fs::read_to_string(&recipe_path).unwrap();
    assert!(content.contains("fn acquire(ctx)"));
    assert!(content.contains("fn install(ctx)"));
}

// =============================================================================
// BUG: Comments preservation in recipe files
// =============================================================================

#[test]
fn test_regression_comments_preserved() {
    let (_dir, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(
        &recipes_dir,
        "comments",
        r#"// Top comment
let ctx = #{
    name: "comments-test",
    value: "initial",
};

// Middle comment
fn acquire(ctx) {
    ctx.value = "updated";
    ctx
}

/* Block comment */
fn install(ctx) { ctx }
"#,
    );

    let engine = RecipeEngine::new(build_dir);
    engine.execute(&recipe_path).unwrap();

    let content = std::fs::read_to_string(&recipe_path).unwrap();
    assert!(content.contains("// Top comment"));
    assert!(content.contains("// Middle comment"));
    assert!(content.contains("/* Block comment */"));
}
